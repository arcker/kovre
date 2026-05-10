# kovre

Orchestrateur de backup self-hosted pour Windows, écrit en Rust.
Configuration déclarative YAML, moteur [`rustic_core`](https://crates.io/crates/rustic_core) (format compatible restic), templates communautaires pour les applications courantes, dashboard web embarqué dans le binaire.

## Statut

- **Phase 1 (moteur CLI)** — terminée.
- **Phase 2 (dashboard web)** — terminée. Sous-commande `kovre serve` qui sert un SPA SvelteKit + l'API JSON Lithair, le tout depuis un seul `kovre.exe`.

Service Windows, scheduler intégré, support VSS, restore via UI, notifications : phases ultérieures.

## Prérequis

- **Rust** stable 1.95+ (épinglé via `rust-toolchain.toml`). 1.92 et plus anciens crashent à la compile sur `rustls` + `aws-lc-rs`.
- **Node 22+** (pour bundler le frontend SvelteKit). Voir `web/.nvmrc`.
- **wasm-pack** (`cargo install wasm-pack`) pour compiler le module WebAssembly du frontend.
- Visual Studio Build Tools — déjà nécessaire pour Rust sur Windows.

## Installation (dev → release)

Build complet du single binary `kovre.exe` (CLI + dashboard) :

```sh
# 1. WASM côté frontend (logique de tri/filtre/validation)
wasm-pack build --target web kovre-wasm

# 2. Bundle SvelteKit
npm --prefix web ci
npm --prefix web run build

# 3. Binaire Rust qui embarque web/build/ via rust-embed
cargo build --release
# → target/release/kovre.exe (~32 MB, autonome)
```

Build CLI seul (sans dashboard, dev backend rapide) :

```sh
cargo build --release      # web/build/.gitkeep créé par build.rs si absent
```

Le binaire compile dans tous les cas — si `web/build/` est vide, le dashboard répond 404 mais les sous-commandes CLI marchent normalement.

## Configuration minimale

Créer un fichier `kovre.yaml` à côté du binaire (ou pointer dessus avec `--config`) :

```yaml
agent:
  data_dir: C:\ProgramData\Kovre
  log_level: info

repositories:
  nas:
    path: \\nas.local\backup\kovre
    password_file: C:\ProgramData\Kovre\nas.key

jobs:
  documents:
    template: documents
    repository: nas
    retention:
      keep_daily: 7
      keep_weekly: 4
```

Voir `kovre.example.yaml` pour un exemple complet avec les trois templates builtin et un job sans template.

Le `password_file` doit exister avant le premier `init-repo` ; il contient la passphrase du dépôt rustic en clair (un mot par ligne, retours à la ligne ignorés). Sécuriser les ACL Windows en conséquence.

## Utilisation

### CLI (Phase 1)

```sh
kovre list-jobs
kovre init-repo nas
kovre run documents
kovre run --all
kovre list-snapshots documents
```

### Dashboard web (Phase 2)

```sh
kovre serve                            # http://127.0.0.1:18080 par défaut
kovre serve --port 9000                # port custom
kovre serve --bind 0.0.0.0             # exposer sur le LAN
kovre serve --debug                    # active le panneau /_admin de Lithair
```

Routes :

- `/` — vue d'ensemble (jobs depuis `kovre.yaml` + dernier run par job)
- `/jobs/:name` — détails d'un job + bouton **Run now** + filtre runs/snapshots
- `/snapshots/:job/:id` — métadonnées d'un snapshot
- `/runs` — historique global (tri par colonne via WebAssembly)
- `/about` — version, health, endpoints

API JSON :

- `GET /api/jobs` — projection read-only de `kovre.yaml::jobs`
- `GET /api/job_runs[/:id]` — historique des runs (CRUD auto-généré par Lithair)
- `GET /api/snapshots[/:id]` — projection des snapshots rustic
- `POST /api/jobs/:name/run` — déclenche un backup, retourne `{"id":"..."}` (202) ; 409 si un run est déjà en cours
- `POST /api/sync` — re-projette les snapshots depuis rustic (pour récupérer ceux créés en CLI sans redémarrer)
- `/health`, `/ready`, `/info` — endpoints opérationnels Lithair

Persistence dashboard : `<agent.data_dir>/lithair/{job_runs,snapshots}/*.raftlog` (event-sourced, replay au boot).

## Templates builtin (Phase 1)

- **documents** — `Documents`, `Desktop`, `Pictures` du profil utilisateur ; exclut `Thumbs.db`, `*.tmp`, `desktop.ini`.
- **dev-repos** — scan d'un dossier racine, prend tout dossier contenant `.git` ; exclut `node_modules`, `target`, `.venv`, `dist`, `build`, `.next`.
- **steam-saves** — détecte Steam via le registre, croise avec le manifest [Ludusavi](https://github.com/mtkennerly/ludusavi-manifest) pour résoudre les chemins de saves des jeux installés.

Un job peut aussi être déclaré sans template : il faut alors fournir `paths` (et optionnellement `excludes`) à la main.

## Limitations explicites

### Phase 1

- **Pas de VSS (Volume Shadow Copy Service).** Les fichiers ouverts en écriture exclusive (Outlook OST, jeux en cours, bases de données live) seront ignorés ou backupés dans un état incohérent. **Lancer la nuit ou navigateurs/jeux fermés est recommandé.**
- **Pas de scheduler intégré.** Utiliser le Planificateur de tâches Windows (`schtasks`) pour automatiser les runs.
- **Pas de service Windows.** Le binaire s'exécute en mode interactif (CLI).
- **Backends : filesystem local et UNC uniquement.** Pas de S3, B2, SFTP, etc.
- **Restore : pas d'UI dédiée.** Utiliser le CLI [`rustic`](https://github.com/rustic-rs/rustic) standard.
- **Watcher filesystem : non.** Les backups sont déclenchés manuellement ou par le scheduler système.
- **Notifications : non.** Surveiller le code de retour et les logs stdout.
- **Fichiers verrouillés : skippés avec un warning.** Pas de panique, le job continue.

### Phase 2 (dashboard)

- **Le YAML reste source de vérité.** Le dashboard ne permet pas d'éditer `kovre.yaml`. Toute modification se fait dans le fichier puis nécessite un redémarrage du serveur.
- **Pas de restore via l'UI.** Les snapshots sont visibles, le dashboard affiche la commande `rustic restore` à utiliser.
- **Pas de logs live d'un run en cours.** Le bouton **Run now** poll toutes les 2s jusqu'à fin. SSE/WebSocket viendront plus tard.
- **Pas d'auth quand `--bind 127.0.0.1`** (default). Pour un bind LAN, prévoir un reverse-proxy authentifié devant — l'auth bearer-token côté kovre n'est pas implémentée en Phase 2.
- **Sync snapshots = boot + on-demand.** Un `kovre run` lancé en CLI pendant que `kovre serve` tourne ne fait PAS apparaître automatiquement le snapshot dans le dashboard ; cliquer le bouton **↻ Refresh** dans le header (équivalent à `POST /api/sync`).
- **CLI vs dashboard décorrélés.** Un `kovre run` CLI crée un snapshot rustic mais **pas** de `JobRun` dans la pipeline dashboard ; seuls les runs déclenchés via `POST /api/jobs/:name/run` apparaissent dans `/runs`.

## Restore

Phase 2 ne fournit toujours pas de commande `restore` côté kovre. Les snapshots étant au format restic standard :

```sh
rustic -r \\nas.local\backup\kovre --password-file C:\ProgramData\Kovre\nas.key restore latest:/ /tmp/restore
```

## Validation manuelle de la compat rustic CLI

Les tests automatisés (`cargo test`) couvrent backup/restore via `rustic_core` directement, sans dépendre du binaire `rustic`. Pour valider que les snapshots produits sont aussi lisibles par la CLI `rustic` standard (DoD Phase 1) :

```sh
# 1. Préparer un repo + un job dans kovre.yaml
kovre init-repo nas
kovre run documents

# 2. Lister via rustic CLI
rustic -r \\nas.local\backup\kovre --password-file C:\ProgramData\Kovre\nas.key snapshots

# 3. Restore le dernier snapshot
rustic -r \\nas.local\backup\kovre --password-file C:\ProgramData\Kovre\nas.key restore latest:/ C:\restore-test

# 4. Diff l'arbo restaurée vs la source originale
robocopy C:\Users\<you>\Documents C:\restore-test\Documents /L /MIR /NJH /NJS
```

## Tests

Le workspace tourne 84 tests :

```sh
cargo test                     # 84 tests, ~3-4 min sur Windows
cargo test --test dashboard    # le e2e du dashboard seul (~1 min)
cargo test --test integration  # les tests Phase 1 (~3 min)
cargo test -p kovre-wasm       # logique de tri WASM (instantané)
```

Le test `dashboard` spawn le binaire kovre, attaque ses endpoints HTTP, et vérifie le flux complet (run → success → snapshot synced → SPA shell servie). Il a besoin de `web/build/` peuplé pour valider la SPA — sinon les assertions sur le shell HTML échouent avec un message qui pointe vers `npm run build`.

## Issues remontées upstream

- [`ISSUES_RUSTIC.md`](ISSUES_RUSTIC.md) — 5 issues sur `rustic_core` (README outdated, exclude semantics, sanitize fail-all, RFC 9557 timestamp, jiff leaked dep).
- [`ISSUES_LITHAIR.md`](ISSUES_LITHAIR.md) — 1 issue sur Lithair, fixée en v0.2.0 (built-in `/health`, `/ready`, `/info`).

## Licence

MIT OR Apache-2.0
