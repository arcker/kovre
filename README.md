# kovre

Orchestrateur de backup self-hosted pour Windows, écrit en Rust.
Configuration déclarative YAML, moteur [`rustic_core`](https://crates.io/crates/rustic_core) (format compatible restic), templates communautaires pour les applications courantes, dashboard web embarqué dans le binaire.

## Statut

- **Phase 1 (moteur CLI)** — terminée.
- **Phase 2 (dashboard web)** — terminée. Sous-commande `kovre serve` qui sert un SPA SvelteKit + l'API JSON Lithair, le tout depuis un seul `kovre.exe`.
- **Phase 3 (édition de config via l'UI)** — terminée. Galerie de templates, wizards par template, écriture atomique de `kovre.yaml`, rechargement à chaud via `ArcSwap` — aucun redémarrage de serveur.

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

## Backends

Chaque `repositories.<name>` a un `backend:` qui choisit comment les fichiers sont stockés. **mirror est le backend recommandé** pour ce qui est l'âme de kovre (photos, documents, mails, jeux) — c'est-à-dire des fichiers que l'utilisateur voudra retrouver tels quels un jour. `rustic` reste disponible pour les cas où la dédup et l'historique snapshot apportent de la valeur (dev trees, log archives, database dumps).

```yaml
repositories:
  photos:                       # backend mirror (recommandé)
    path: \\nas.local\photos-mirror
    backend: mirror

  dev:                          # backend rustic
    path: \\nas.local\backup\kovre
    backend: rustic
    password_file: C:\ProgramData\Kovre\dev.key

jobs:
  family-photos:
    repository: photos
    paths:
      - D:\Pictures
    retention:
      keep_versions: 5          # spécifique mirror

  code:
    repository: dev
    template: dev-repos
    template_options:
      scan_root: D:\dev
    retention:
      keep_daily: 7              # spécifique rustic
```

| Backend | Format | Cible | Restore | Verify |
|---|---|---|---|---|
| **mirror** (recommandé) | fichiers natifs 1:1 sous `<repo>/<job>/<basename>/`, versions archivées dans `<repo>/<job>/.versions/<rel>/<stem>-<ts>.<ext>` | photos, documents, mails (Thunderbird), profils navigateurs, sauvegardes de jeux — tout ce qu'on veut pouvoir browser/copier direct depuis Explorer | `cp` direct depuis la destination, ou `restore_latest` côté kovre | no-op (fichiers natifs, l'OS garantit la lisibilité) |
| **rustic** | restic-compatible : encrypted, dédupliqué, snapshots immutables | dev trees, log archives, database dumps — tout ce qui profite de la dédup et n'a pas besoin d'être browsable en plain | `restore_latest` côté kovre ou CLI `rustic` standard | `repository.check()` (metadata + index) |

> **Compat YAML** : si `backend:` est omis, on garde la valeur historique `rustic` pour ne pas casser les configs antérieures à Phase 4. Le wizard du dashboard, en revanche, propose `mirror` en premier.

Détection de changement côté mirror : `mtime + size`, avec un **second passage par hash SHA-256** pour reconnaître les renames (un fichier déplacé dans un sous-dossier ne fait pas exploser `.versions/`). Les fichiers de plus de 512 MiB sautent le hash (re-lire un MKV de 4K pour économiser une copie est une mauvaise affaire). `.versions/` est un nom réservé — un dossier source qui contient `.versions/` à la racine est refusé pour éviter l'auto-collision.

Retention :
- **mirror** : `keep_versions` (par fichier canonique dans `.versions/`).
- **rustic** : `keep_last`, `keep_hourly`, `keep_daily`, `keep_weekly`, `keep_monthly`, `keep_yearly` (sur les snapshots).

### Authentification SMB pour les UNC

Si le `path` d'un repository est un UNC vers un share réseau (`\\diskstation\disque\kovre`), trois cas :

1. **Session Windows déjà authentifiée** sur le share (mappage manuel, Credential Manager, GPO logon) → kovre y accède directement, rien à configurer.
2. **Share anonyme** → idem.
3. **Credentials explicites nécessaires** → ajouter `smb_user` et `smb_password_file` au repo :

```yaml
repositories:
  nas:
    path: \\diskstation\disque\kovre
    backend: mirror
    smb_user: kovre-backup
    smb_password_file: C:\ProgramData\Kovre\nas.smb.dpapi
```

Le `smb_password_file` pointe sur un **blob chiffré via Windows DPAPI** (scope `CurrentUser`), pas du texte clair. Pour le créer : ouvrir l'édition du repo dans le dashboard, remplir `smb_user` + le chemin cible, taper le password dans le champ "Set SMB password" et cliquer **Store**. Le password est envoyé au serveur local en POST (jamais persisté), chiffré côté serveur via `CryptProtectData`, et seul le blob ciphered atterrit sur disque. **Seul ton utilisateur Windows sur cette machine peut le déchiffrer** — copie sur un autre PC = inutilisable.

Au démarrage de `kovre serve`, kovre :
1. Lit le blob, le déchiffre via DPAPI.
2. Extrait `\\diskstation\disque` du UNC.
3. Appelle `WNetAddConnection2` (Win32) avec les credentials pour authentifier la session courante.
4. Le password vit ~10 ms en RAM puis est zeroized.

**Sécurité critique** :
- **ACL NTFS sur le fichier `.dpapi`** : restreindre à ton user uniquement (`icacls C:\ProgramData\Kovre\nas.smb.dpapi /inheritance:r /grant:r %USERNAME%:R`). Le blob est inutilisable par un autre user, mais autant éviter de le laisser lisible.
- **SMB 3+ avec encryption sur le NAS** : SMB 1/2 doivent être désactivés côté serveur. Sinon le password est transmis en clair sur le wire à chaque op.
- **`--bind 0.0.0.0` est interdit** sans reverse-proxy authentifié : la route `POST /api/repositories/store-smb-password` permettrait à n'importe qui sur le LAN de stocker un blob arbitraire chiffré avec ta clé DPAPI. Sur `127.0.0.1` (le défaut), c'est sécurisé par l'isolation de la machine.

## Restore (Phase 6)

Depuis le dashboard, chaque job affiche un bouton **↻ Restore** dans la vue inventaire (home). Cliquer ouvre `/jobs/<name>/restore` :

1. Choisir un dossier de destination (pré-rempli `C:\kovre-restore\<job>\<date>`, éditable).
2. Cliquer **Restore** — le serveur retourne 202, la page poll `GET /api/restore_runs/<id>` toutes les 2 s avec une barre de progression.
3. Le restore copie la dernière version canonique depuis le repository vers le dossier choisi. **Les sources originales ne sont jamais touchées.**
4. Statut terminal : ✓ succès (avec le chemin) ou ✗ erreur avec la raison.

Pour le moment, seul le **latest state** est restaurable (pas de picker de snapshot/version — Phase 7).

Fonctionnement par backend :
- **mirror** : copie `<repo>/<job>/<basename>/…` vers la destination (`.versions/` est exclu — seule la version canonique courante est restaurée). Très rapide.
- **rustic** : ouvre le repo, sélectionne le snapshot le plus récent tagué `kovre-job:<name>`, et le décompresse via `rustic_core::prepare_restore + restore`. La passphrase est lue automatiquement depuis `password_file`.

Restore manuel (power-users) :
- **rustic CLI** : `rustic -r \\nas.local\backup\kovre --password-file C:\ProgramData\Kovre\dev.key restore latest:/ C:\restore-test`
- **mirror** : les fichiers sont natifs, `robocopy` / Explorer / `xcopy` fonctionne. `.versions/<rel>/<stem>-<ts>.<ext>` contient les versions antérieures.

Fiabilité : le test e2e `cargo test restore_round_trip` vérifie l'égalité octet-à-octet (backup → restore → diff) sur un fixture varié (texte, binaire, accents, espaces, imbrication) pour les deux backends. Le test dashboard `dashboard_end_to_end` exerce le cycle complet via API (run → restore → diff on disk).

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

- `/` — vue d'ensemble (tiles par job + dernier run + bouton **Run now**)
- `/jobs/:name` — détails d'un job + filtre runs/snapshots
- `/snapshots/:job/:id` — métadonnées d'un snapshot
- `/runs` — historique global (tri par colonne via WebAssembly)
- `/templates` — galerie des 4 templates pour ajouter un nouveau job
- `/templates/:name` — wizard du template, écrit `kovre.yaml` et recharge à chaud
- `/about` — version, health, endpoints

API JSON :

- `GET /api/jobs` — projection read-only de `kovre.yaml::jobs`
- `GET /api/job_runs[/:id]` — historique des runs (CRUD auto-généré par Lithair)
- `GET /api/snapshots[/:id]` — projection des snapshots rustic
- `GET /api/config` — `{yaml, parsed}` reflétant la config en mémoire
- `GET /api/templates` — catalogue des 7 templates (user-files, thunderbird-mail, browser-profiles, dev-repos, steam-saves, user-appdata, custom) avec leur schéma d'options
- `POST /api/templates/:name/resolve` — résout un template en paths concrets + excludes sur cette machine (body JSON : options du template)
- `GET /api/fs?path=<dir>` — liste les sous-dossiers de `<dir>` (autocomplete du picker)
- `POST /api/jobs/:name/run` — déclenche un backup, retourne `{"id":"..."}` (202) ; 409 si un run est déjà en cours
- `POST /api/jobs/:name/restore` — déclenche un restore (body JSON : `{dest_dir}`) ; 202 + `{id}` (poll via `GET /api/restore_runs/:id`) ; 404 si job inconnu, 400 si dest invalide, 409 si déjà en cours
- `GET /api/restore_runs[/:id]` — historique des restores (CRUD auto-généré par Lithair)
- `POST /api/repositories/:name/verify` — check d'intégrité (rustic : metadata + index ; mirror : no-op informant)
- `POST /api/sync` — re-projette les snapshots depuis rustic (pour récupérer ceux créés en CLI sans redémarrer)
- `PUT /api/config` — accepte un YAML brut, valide via `Config::from_str`, écrit atomiquement et swap l'`ArcSwap` ; 400 avec `{error, message, location: {line, column}}` si invalide
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

- **Pas de restore via l'UI.** Les snapshots sont visibles, le dashboard affiche la commande `rustic restore` à utiliser.
- **Pas de logs live d'un run en cours.** Le bouton **Run now** poll toutes les 2s jusqu'à fin. SSE/WebSocket viendront plus tard.
- **⚠ Pas d'auth quand `--bind 0.0.0.0`** (LAN). **Ne pas utiliser `--bind 0.0.0.0` sans un reverse-proxy authentifié devant.** Les routes exposées (`PUT /api/config`, `POST /api/jobs/:name/run`, `POST /api/jobs/:name/restore`, `POST /api/repositories/init-password`, `POST /api/repositories/store-smb-password`) permettent respectivement de réécrire la config, lancer un backup (= lecture arbitraire de fichiers), lancer un restore (= écriture arbitraire sur disque), créer un fichier à un chemin arbitraire, et **stocker un blob DPAPI** (= encryption d'un secret arbitraire avec ta clé utilisateur). Sur `127.0.0.1` (le défaut) c'est sécurisé par l'isolation de la machine. Sur LAN, tout le réseau local y a accès. L'auth bearer-token côté kovre n'est pas encore implémentée.
- **Sync snapshots = boot + on-demand.** Un `kovre run` lancé en CLI pendant que `kovre serve` tourne ne fait PAS apparaître automatiquement le snapshot dans le dashboard ; cliquer le bouton **↻ Refresh** dans le header (équivalent à `POST /api/sync`).
- **CLI vs dashboard décorrélés.** Un `kovre run` CLI crée un snapshot rustic mais **pas** de `JobRun` dans la pipeline dashboard ; seuls les runs déclenchés via `POST /api/jobs/:name/run` apparaissent dans `/runs`.

### Phase 3 (édition config via l'UI)

- **Le YAML reste l'artefact final** — la dashboard l'écrit mais le fichier est aussi éditable à la main avec n'importe quel éditeur.
- **Édition raw du YAML non exposée** — la galerie `/templates` permet d'ajouter des jobs, mais il n'y a pas de textarea libre dans l'UI. Pour modifier `agent:`, `repositories:`, ou supprimer un job existant : éditer `kovre.yaml` à la main. Le serveur recharge à chaque PUT, mais une modif disque externe demande encore un restart (`--watch-config` est non implémenté).
- **Comments perdus à l'édition UI.** Un PUT depuis le dashboard remplace le fichier ; les commentaires manuels du YAML disparaissent. Le block ajouté par le wizard reste en forme canonique (2-space indent, scalars quotés sur Windows paths / globs).
- **Modification / suppression de jobs existants** non exposée dans l'UI — le wizard ne sait qu'ajouter. À la main dans le YAML pour le reste.
- **Validation YAML côté client minimale** — le wizard vérifie juste nom unique + champs requis. La vraie validation (champ inconnu, repo référencé inexistant, etc.) revient du serveur en `400` avec ligne/colonne.

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

Le workspace tourne plusieurs suites (Phase 1 → 5) — count exact via `cargo test --workspace --exclude kovre-wasm 2>&1 | rg "test result"`.

```sh
cargo test                                            # toutes les suites, ~5 min sur Windows
cargo test --test dashboard                           # le e2e du dashboard seul (~80 s)
cargo test --test integration                         # les tests Phase 1+4 (inclut restore_round_trip)
cargo test -p kovre-core --lib backup::mirror         # juste les unit tests du moteur mirror (rapide)
cargo test -p kovre-wasm                              # logique de tri WASM (instantané)
```

Le test `dashboard` spawn le binaire kovre, attaque ses endpoints HTTP, et vérifie le flux complet : run rustic → success → snapshot synced → SPA shell servie, puis Phase 3 (GET /api/templates, GET /api/fs, PUT /api/config valide → live reload, PUT /api/config invalide → 400 sans mutation), puis Phase 4 (verify route ; pipeline mirror complet : PUT /api/config avec `backend: mirror`, init, 4 runs séquentiels avec modifications source, asserts `.versions/` et retention `keep_versions`). Il a besoin de `web/build/` peuplé pour valider la SPA — sinon les assertions sur le shell HTML échouent avec un message qui pointe vers `npm run build`.

`cargo test --test integration restore_round_trip` couvre la promesse de fiabilité (backup → restore → diff octet-à-octet sur fixture variée) pour les deux backends.

## Issues remontées upstream

- [`ISSUES_RUSTIC.md`](ISSUES_RUSTIC.md) — 5 issues sur `rustic_core` (README outdated, exclude semantics, sanitize fail-all, RFC 9557 timestamp, jiff leaked dep).
- [`ISSUES_LITHAIR.md`](ISSUES_LITHAIR.md) — 6 issues filed sur Lithair, toutes fixées entre v0.2.0 et v0.6.0 (built-in `/health`/`/ready`/`/info` ; `response::json_value` ; `query::param` ; `RouteRequest`/`RouteResponse` + `with_route_async` ; `response::builder()` + `with_not_found_handler_async` ; `request::read_body{,_with_limit,_as_string,_json}`). kovre n'a aucune dépendance directe sur les couches sous Lithair.

## Licence

MIT OR Apache-2.0
