# kovre

Orchestrateur de backup self-hosted pour Windows, écrit en Rust.
Configuration déclarative YAML, moteur [`rustic_core`](https://crates.io/crates/rustic_core) (format compatible restic), templates communautaires pour les applications courantes.

> **Phase 1 : moteur CLI uniquement.** Le service Windows, le dashboard web, le scheduler intégré et le support VSS arrivent dans des phases ultérieures. Le binaire produit aujourd'hui est utilisable manuellement ou via le Planificateur de tâches Windows.

## Statut

Phase 1 — en cours de développement. Voir `init.md` pour le scope détaillé.

## Installation (dev)

```sh
cargo build --release
# Le binaire est dans target/release/kovre.exe
```

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

```sh
kovre list-jobs
kovre init-repo nas
kovre run documents
kovre run --all
kovre list-snapshots documents
```

## Templates builtin (Phase 1)

- **documents** — `Documents`, `Desktop`, `Pictures` du profil utilisateur ; exclut `Thumbs.db`, `*.tmp`, `desktop.ini`.
- **dev-repos** — scan d'un dossier racine, prend tout dossier contenant `.git` ; exclut `node_modules`, `target`, `.venv`, `dist`, `build`, `.next`.
- **steam-saves** — détecte Steam via le registre, croise avec le manifest [Ludusavi](https://github.com/mtkennerly/ludusavi-manifest) pour résoudre les chemins de saves des jeux installés.

Un job peut aussi être déclaré sans template : il faut alors fournir `paths` (et optionnellement `excludes`) à la main.

## Limitations explicites (Phase 1)

- **Pas de VSS (Volume Shadow Copy Service).** Les fichiers ouverts en écriture exclusive (Outlook OST, jeux en cours, bases de données live) seront ignorés ou backupés dans un état incohérent. **Lancer la nuit ou navigateurs/jeux fermés est recommandé.**
- **Pas de scheduler intégré.** Utiliser le Planificateur de tâches Windows (`schtasks`) pour automatiser les runs.
- **Pas de service Windows.** Le binaire s'exécute en mode interactif (CLI).
- **Backends : filesystem local et UNC uniquement.** Pas de S3, B2, SFTP, etc. dans cette phase.
- **Restore : pas d'UI dédiée.** Utiliser le CLI [`rustic`](https://github.com/rustic-rs/rustic) standard (les snapshots sont compatibles).
- **Watcher filesystem : non.** Les backups sont déclenchés manuellement ou par le scheduler système.
- **Notifications : non.** Surveiller le code de retour et les logs stdout.
- **Fichiers verrouillés : skippés avec un warning.** Pas de panique, le job continue.

## Restore

Phase 1 ne fournit pas de commande `restore` propre. Les snapshots étant au format restic standard, utiliser :

```sh
rustic -r \\nas.local\backup\kovre --password-file C:\ProgramData\Kovre\nas.key restore latest:/ /tmp/restore
```

## Licence

MIT OR Apache-2.0
