# Kovre — Phase 2 : Dashboard Web (Lithair)

## Contexte produit

Phase 1 a livré le moteur CLI (cf. `init.md`, terminée le 2026-05-03). Phase 2 ajoute un dashboard web embarqué dans le même binaire pour donner :

- visibilité sur les jobs configurés,
- liste des snapshots par job,
- historique des runs (succès/échec/durée/bytes),
- déclenchement manuel d'un backup depuis l'UI.

**Philosophie héritée de Phase 1 :**
- Backup-as-code : `kovre.yaml` reste la source de vérité immuable de la configuration. Phase 2 ne permet PAS d'éditer la config via l'UI.
- AI-agent-friendly : les API REST exposées par le dashboard sont auto-générées et introspectables.
- Single binary : tout le frontend (HTML/JS/WASM) est embarqué dans le `kovre.exe` final.

**Nouveau paradigme :** dogfood de [Lithair](https://github.com/lithair/lithair) comme infra serveur + framework de modèles déclaratifs. Lithair gère HTTP, persistence event-sourced (`.raftlog`), CRUD auto, RBAC, /health/ready/info. On écrit uniquement la logique kovre-spécifique (custom routes pour trigger backup, modèles `JobRun`/`Snapshot`, frontend dédié).

**Procédure bug Lithair :** au moindre bug rencontré, on ouvre une issue dans `lithair/lithair` et on tracke dans `ISSUES_LITHAIR.md` côté kovre (même pattern qu'`ISSUES_RUSTIC.md`). Si bloquant : on attend le fix ou on workaround temporairement avec commentaire `// TODO(lithair#NN): retirer ce workaround`.

## Stack imposé

- **Backend serveur :** `lithair-core = "0.1"` (Hyper-based)
- **Frontend :** SvelteKit en mode SPA (adapter-static), bundlé avec Vite, servi statiquement par Lithair en mémoire
- **Logique UI :** Rust compilé en WASM via `wasm-bindgen` + `wasm-pack`. Pas de JS pour les opérations sur données (sort, filter, search, validate). Svelte ne fait que le rendering et le routage.
- **Crate workspace** (nouveau découpage) :
  - `kovre-core` — lib : `config`, `backup`, `ludusavi`, `templates` (extrait de la lib actuelle)
  - `kovre` — bin : sous-commandes CLI existantes (`run`, `list-jobs`, `list-snapshots`, `init-repo`) **+** nouvelle sous-commande `serve`
  - `kovre-wasm` — cdylib : exports WASM (sort, filter, validate, retention preview)
- **Embedding du frontend :** `rust-embed` sur le dossier `web/build/` ; Lithair sert les assets memory-first
- **Build pipeline :** `cargo build --release` build kovre. Pour bundler le frontend, étape préalable : `npm --prefix web ci && npm --prefix web run build` (à scripter dans un `task` ou `xtask`). Première régression sur Phase 1 : on introduit Node dans le build de release.

## Scope Phase 2

### Inclus

- Sous-commande `kovre serve [--port 18080] [--bind 127.0.0.1] [--debug]`
- Modèles Lithair : `JobRun`, `Snapshot`, `Setting`
- Sync `kovre.yaml` → modèles `Repository`/`Job` Lithair en lecture seule au démarrage (refresh sur SIGHUP ou flag `--watch-config`)
- Custom routes :
  - `POST /api/jobs/:name/run` — déclenche un backup, crée un `JobRun`, exécute en task tokio détachée, retourne 202 Accepted avec l'id du run
  - `GET /api/jobs/:name/snapshots` — proxy lecture vers `rustic_core` (cache éventuel via modèle `Snapshot`)
- Frontend SvelteKit (5 routes) :
  - `/` — vue d'ensemble (jobs + last run status + bouton run)
  - `/jobs/:name` — détails d'un job, list snapshots, list runs, bouton run
  - `/snapshots/:job/:id` — métadonnées d'un snapshot (paths, summary, hostname)
  - `/runs` — historique global, filtrable
  - `/about` — version, build info, lien vers les logs
- WASM exports (kovre-wasm) :
  - `sort_runs_by(runs, key, dir)`, `filter_runs(runs, predicate)`, `search_jobs(jobs, query)`
  - `validate_yaml(text)` — parse + retourne erreurs structurées (réutilise `kovre_core::config`)
  - `retention_preview(snapshots, policy)` — utilise `KeepOptions::apply` côté client
- Endpoints ops Lithair : `/health`, `/ready`, `/info`
- Admin-UI Lithair (`/_admin/*`) : activée seulement avec `--debug` (côté kovre, pas côté Lithair feature)
- Auth : aucune en bind 127.0.0.1 ; token bearer dans un fichier (`agent.dashboard_token_file` dans kovre.yaml) si bind != localhost

### Exclus (Phases ultérieures)

- Restore via l'UI (Phase 3)
- Edit kovre.yaml via l'UI (Phase 3)
- Logs live d'un run en cours (SSE / WebSocket) — Phase 3
- Notifications mail/Discord/webhook — Phase 3
- Scheduler intégré (cron) — Phase 4
- VSS — Phase 4
- Service Windows — phase finale (après validation features)
- Backends additionnels (S3, B2, SFTP) — Phase 5
- Multi-utilisateur, OAuth — non prévu
- Mode cluster Lithair (OpenRaft) — non prévu pour kovre

## Modèles Lithair

```rust
#[derive(DeclarativeModel, Serialize, Deserialize, Clone, Debug)]
pub struct JobRun {
    #[db(primary_key, indexed)]
    #[http(expose)]
    #[lifecycle(immutable)]
    pub id: Uuid,

    #[db(indexed)]
    #[http(expose)]
    #[lifecycle(immutable)]
    pub job_name: String,

    #[http(expose)]
    pub started_at: jiff::Zoned,

    #[http(expose)]
    pub finished_at: Option<jiff::Zoned>,

    #[http(expose)]
    pub status: RunStatus,            // Running | Success | Failed { reason: String }

    #[http(expose)]
    pub snapshot_id: Option<String>,

    #[http(expose)]
    pub bytes_processed: Option<u64>,
    #[http(expose)]
    pub bytes_added: Option<u64>,

    #[http(expose)]
    pub trigger: TriggerSource,       // Cli | Dashboard | Scheduled (futur)
}

#[derive(DeclarativeModel, ...)]
pub struct Snapshot {
    #[db(primary_key)]    pub id: String,           // rustic snapshot id
    #[db(indexed)]        pub job_name: String,
                          pub time: jiff::Zoned,
                          pub paths: Vec<String>,
                          pub bytes_total: Option<u64>,
                          pub hostname: String,
}

#[derive(DeclarativeModel, ...)]
pub struct Setting {
    #[db(primary_key)]    pub key: String,          // ex: "dashboard.last_seen_run_id"
                          pub value: serde_json::Value,
}
```

`Repository` et `Job` ne sont **pas** des modèles Lithair (read-only depuis le YAML, pas de writes API).

## Format YAML — additions Phase 2

```yaml
agent:
  data_dir: C:\ProgramData\Kovre
  log_level: info
  # Phase 2 :
  dashboard:
    enabled: true                         # default false (opt-in)
    bind: 127.0.0.1
    port: 18080
    raftlog_dir: C:\ProgramData\Kovre\lithair  # event-sourced state lives here
    token_file: null                      # required if bind != 127.0.0.1
```

Tout est optionnel ; si `dashboard:` absent, `kovre serve` échoue avec un message clair.

## Arborescence cible (post-restructure)

```
kovre/
├── Cargo.toml                      # [workspace]
├── kovre.example.yaml
├── README.md
├── ISSUES_RUSTIC.md
├── ISSUES_LITHAIR.md               # nouveau
├── init.md
├── init-phase-2.md
├── kovre-core/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── config.rs
│       ├── backup.rs
│       ├── ludusavi.rs
│       └── templates/
├── kovre/                          # bin
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs
│       ├── cli.rs
│       └── serve/                  # nouveau module Phase 2
│           ├── mod.rs
│           ├── models.rs
│           ├── routes.rs
│           └── sync.rs             # kovre.yaml -> Lithair models
├── kovre-wasm/
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs                  # cdylib, exports wasm-bindgen
├── web/                            # SvelteKit
│   ├── package.json
│   ├── svelte.config.js            # adapter-static
│   ├── vite.config.ts              # imports kovre-wasm via vite-plugin-wasm-pack
│   ├── src/
│   │   ├── app.html
│   │   ├── lib/
│   │   │   └── api.ts              # client typé pour les routes Lithair auto-générées
│   │   └── routes/
│   │       ├── +layout.svelte
│   │       ├── +page.svelte        # /
│   │       ├── jobs/[name]/+page.svelte
│   │       ├── snapshots/[job]/[id]/+page.svelte
│   │       ├── runs/+page.svelte
│   │       └── about/+page.svelte
│   └── build/                      # produit par vite build, embarqué via rust-embed
└── tests/
    ├── integration.rs              # existant Phase 1
    └── dashboard.rs                # nouveau — tests e2e du serve
```

## Étapes (ordre)

Étapes séquentielles. **Validation utilisateur entre chaque** (même règle que Phase 1).

1. **Workspace split** : transformer `kovre/` en workspace, extraire `kovre-core/` (lib pure, sans clap, sans tracing-subscriber init), garder `kovre/` comme bin (CLI). Faire passer tous les tests existants. Pas de Lithair encore.

2. **Sous-commande `serve` squelette** : ajouter `Command::Serve` à clap, ajouter dépendance `lithair-core`, faire `LithairServer::new().with_port(...).serve()` minimal qui répond `/health`. Pas de modèles encore.

3. **Modèle `JobRun`** : déclarer le modèle, le brancher dans `LithairServer::with_model`, valider que `GET /api/job_runs` répond, écrire un test qui POST un `JobRun` et le relit.

4. **Custom route `POST /api/jobs/:name/run`** : valide que le job existe dans `kovre.yaml`, crée un `JobRun` en status `Running`, spawn une tokio task qui appelle `kovre_core::backup::backup_job`, met à jour le `JobRun` à la fin. Test unitaire : POST → 202 + run_id, polling jusqu'à status Success.

5. **Modèle `Snapshot` + sync read-only depuis rustic** : au démarrage et sur demande, énumérer les snapshots de chaque job via `rustic_core` et matérialiser dans le modèle Lithair. Endpoint `GET /api/snapshots?job_name=foo`.

6. **Crate `kovre-wasm`** : créer la crate cdylib avec `wasm-bindgen`. Premier export : `sort_runs_by(runs_json, key, dir) -> runs_json`. Build via `wasm-pack build --target web`.

7. **SvelteKit setup** : créer `web/`, configurer adapter-static, vite-plugin-wasm-pack, page `/` qui fetch `GET /api/job_runs` et affiche une table sans tri pour l'instant. Lance `npm run dev` avec proxy vers `kovre serve` sur :18080.

8. **WASM dans Svelte** : la table de la page `/` utilise `sort_runs_by` côté client sur clic d'en-tête de colonne. Aucun JS de tri custom — uniquement le binding wasm.

9. **Routes Svelte restantes** : `/jobs/:name`, `/snapshots/:job/:id`, `/runs`, `/about`. Bouton "Run job" sur `/jobs/:name` qui POST puis polle.

10. **Embedding production** : `rust-embed` sur `web/build/`, Lithair sert depuis la mémoire. Vérifier qu'`cargo build --release` après `npm run build` produit un binaire autonome qui marche.

11. **Validation manuelle + admin-ui debug + tests d'intégration** : test e2e dans `tests/dashboard.rs` qui spawn le binaire avec `serve --debug`, fait des requêtes HTTP, valide les flux principaux. Documenter dans README la procédure de build complet (npm + cargo).

## Definition of Done

- [ ] `cargo build --release` produit un binaire qui marche sur Windows 11
- [ ] `npm --prefix web ci && npm --prefix web run build` produit `web/build/` qui est embarqué via rust-embed
- [ ] `kovre serve` démarre sur 127.0.0.1:18080 par défaut, sert le frontend Svelte et les API Lithair
- [ ] Sous-commandes CLI Phase 1 (`run`, `list-jobs`, `list-snapshots`, `init-repo`) marchent encore — pas de régression
- [ ] La page `/` liste les jobs depuis kovre.yaml et le statut du dernier run de chacun
- [ ] La page `/jobs/:name` permet de déclencher un backup ; le run apparaît en historique avec status final correct
- [ ] La page `/snapshots/:job/:id` affiche les métadonnées d'un snapshot
- [ ] Les listes affichées (runs, snapshots) sont triables/filtrables, **et le tri est exécuté en WASM, pas en JS** (vérifiable en lisant le code Svelte : aucune fonction `sort()` JS sur les data arrays côté UI)
- [ ] L'event-store `.raftlog` survit à un redémarrage : les runs historiques restent visibles
- [ ] `--debug` active l'admin-ui Lithair sur `/_admin/*`
- [ ] `--bind 0.0.0.0` exige un `dashboard.token_file` configuré, échoue gracieusement sinon
- [ ] `ISSUES_LITHAIR.md` créé, vide ou contenant les issues remontées pendant le dev
- [ ] README mis à jour avec : build pipeline (npm + cargo), commande `kovre serve`, captures d'écran (optionnel)

## Contraintes de qualité

- **Pas de JS pour la logique de données** : toute opération de tri/filtre/validation passe par WASM. Si une fonction JS native (Array.prototype.sort, filter, find) est utilisée sur des données métier (runs, snapshots, jobs), c'est un défaut.
- **Bundle frontend total < 300 KB compressé** (HTML+JS+WASM). À mesurer en fin de Phase 2 ; si dépassé, réduire avant DoD.
- **Bundle WASM < 150 KB compressé.** Optimisations à activer : `wasm-opt -Oz`, `panic_immediate_abort`, no `std::fmt`. Si dépassé, profiler et alléger.
- **Démarrage `kovre serve` < 500 ms** sur Windows 11 NVMe (cold start, sans replay massif d'événements).
- **API REST autodocumentée** : chaque endpoint custom (POST /api/jobs/:name/run) doit avoir un doc-comment + un test d'intégration.
- **Concurrence backup** : si l'utilisateur clique "Run" sur un job déjà en cours, retourner 409 Conflict avec l'id du run en cours. Pas de doubles runs sur le même job.
- **Erreurs visibles** : tout `JobRun` en `Failed { reason }` doit afficher la raison dans l'UI sans tronquer.
- **Pas de panique sur kovre.yaml absent** au démarrage de `serve` : message d'erreur clair, exit code dédié.
- **Localhost par défaut, pas d'auth** : préserve le UX dev. Bind LAN exige du token.
- **Compat Phase 1** : `kovre.yaml` Phase 1 sans bloc `dashboard:` doit continuer à parser et faire tourner les sous-commandes CLI.

## Instruction d'exécution pour Claude Code

Procède étape par étape (1 à 11). À la fin de chaque étape, fais un point bref : ce qui a été fait, ce qui a été décidé en cours de route (notamment toute découverte sur l'API Lithair qui pourrait remettre en cause un choix d'archi), et attends la validation user avant de passer à la suivante. Pas d'enchaînement automatique.

Si un bug Lithair bloque une étape : remonter une issue dans `lithair/lithair`, ajouter une ligne dans `ISSUES_LITHAIR.md`, demander à l'utilisateur s'il préfère attendre le fix ou workaround.
