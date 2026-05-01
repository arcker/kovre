\# Kovre — Phase 1 : Moteur CLI



\## Contexte produit



\*\*Nom :\*\* `kovre`



\*\*Pitch :\*\* Orchestrateur de backup self-hosted en Rust pour Windows, config déclarative YAML, templates communautaires pour les applications courantes. À terme : service unique avec dashboard web embarqué.



\*\*Philosophie :\*\*

\- Un binaire, un fichier de config, zéro structure imposée

\- AI-agent-friendly : le YAML est le contrat d'API

\- Backup-as-code

\- N'invente rien : `rustic\_core` comme moteur, consomme le manifest Ludusavi pour les jeux



\*\*Stack imposé :\*\*

\- Rust stable (édition 2021)

\- `rustic\_core` (moteur backup, format compatible restic)

\- `serde` + `serde\_yaml` (config)

\- `clap` v4 derive (CLI)

\- `tracing` + `tracing-subscriber` (logging)

\- `tokio` runtime

\- `walkdir` pour le scan filesystem

\- `reqwest` pour le téléchargement du manifest Ludusavi

\- Pas de framework web cette phase



\## Scope Phase 1



\*\*Objectif :\*\* valider le moteur. CLI qui prend un YAML et exécute les jobs.



\*\*Inclus :\*\*

\- Parsing YAML

\- 3 templates builtin : `documents`, `dev-repos`, `steam-saves`

\- Téléchargement + cache du manifest Ludusavi (https://raw.githubusercontent.com/mtkennerly/ludusavi-manifest/master/data/manifest.yaml) avec ETag

\- Intégration `rustic\_core` : init repo, snapshot, list, retention

\- CLI : `run <job>`, `run --all`, `list-jobs`, `list-snapshots <job>`, `init-repo <repo>`

\- Logging structuré stdout



\*\*Exclus (phases ultérieures) :\*\*

\- Service Windows

\- Dashboard web (Lithair)

\- Scheduler (cron)

\- VSS (Volume Shadow Copy)

\- Restore via UI (utiliser le CLI rustic standard)

\- Notifications

\- Backends autres que filesystem/UNC

\- Watcher filesystem



\## Format YAML cible



Fichier par défaut : `./kovre.yaml` (override via `--config`).



```yaml

agent:

&#x20; data\_dir: C:\\ProgramData\\Kovre

&#x20; log\_level: info



repositories:

&#x20; nas:

&#x20;   path: \\\\nas.local\\backup\\kovre

&#x20;   password\_file: C:\\ProgramData\\Kovre\\nas.key



jobs:

&#x20; documents:

&#x20;   template: documents

&#x20;   repository: nas

&#x20;   retention:

&#x20;     keep\_daily: 7

&#x20;     keep\_weekly: 4

&#x20;     keep\_monthly: 12



&#x20; dev:

&#x20;   template: dev-repos

&#x20;   repository: nas

&#x20;   template\_options:

&#x20;     scan\_root: D:\\dev

&#x20;   retention:

&#x20;     keep\_last: 30



&#x20; steam:

&#x20;   template: steam-saves

&#x20;   repository: nas

&#x20;   retention:

&#x20;     keep\_last: 10



&#x20; custom-photos:              # job sans template

&#x20;   repository: nas

&#x20;   paths:

&#x20;     - D:\\Photos

&#x20;   excludes:

&#x20;     - "\*\*/\*.tmp"

&#x20;   retention:

&#x20;     keep\_last: 50

```



\## Templates builtin



Trait commun :



```rust

pub trait Template {

&#x20;   fn name(\&self) -> \&'static str;

&#x20;   fn resolve(\&self, options: \&serde\_yaml::Value) -> Result<ResolvedTemplate>;

}



pub struct ResolvedTemplate {

&#x20;   pub paths: Vec<PathBuf>,

&#x20;   pub excludes: Vec<String>,

}

```



\- \*\*documents\*\* : `%USERPROFILE%\\Documents`, `Desktop`, `Pictures` ; excludes `\*\*/Thumbs.db`, `\*\*/\*.tmp`, `\*\*/desktop.ini`

\- \*\*dev-repos\*\* : scan `scan\_root` (défaut `%USERPROFILE%`), trouve les dossiers contenant `.git`, exclut `node\_modules`, `target`, `.venv`, `dist`, `build`, `.next`

\- \*\*steam-saves\*\* : parse manifest Ludusavi en cache, détecte Steam via `HKLM\\SOFTWARE\\Valve\\Steam` (clé `InstallPath`), scanne `steamapps/common/`, croise avec le manifest pour résoudre les chemins de saves



\## Arborescence



```

kovre/

├── Cargo.toml

├── README.md

├── kovre.example.yaml

├── src/

│   ├── main.rs

│   ├── config.rs

│   ├── cli.rs

│   ├── backup.rs              # wrapper rustic\_core

│   ├── ludusavi.rs            # download + cache + parse

│   └── templates/

│       ├── mod.rs             # trait + registry

│       ├── documents.rs

│       ├── dev\_repos.rs

│       └── steam\_saves.rs

└── tests/

&#x20;   └── integration.rs

```



Crate unique pour V1.



\## Étapes (ordre)



1\. `cargo new kovre`, dépendances, README avec scope, `.gitignore`

2\. Structs `Config`/`Repository`/`Job` + parsing + tests unitaires sur YAML d'exemple

3\. Squelette CLI clap + commande `list-jobs`

4\. Trait `Template` + impl `documents` (le plus simple)

5\. Intégration `rustic\_core` : init-repo, backup d'un job, list-snapshots ; valider compat avec CLI rustic standard

6\. Template `dev-repos`

7\. Module `ludusavi` (download + ETag cache) + template `steam-saves`

8\. Retention (rustic le supporte nativement, juste à mapper la config)

9\. Tests d'intégration : dossier temp → backup → restore via rustic CLI → diff



\## Definition of Done



\- \[ ] `cargo build --release` produit un binaire qui marche sur Windows 11

\- \[ ] Un YAML avec 1 repo + 3 jobs (un par template) s'exécute sans erreur

\- \[ ] Les snapshots produits sont lisibles par `rustic` (CLI standard) — compatibilité format validée

\- \[ ] Retention appliquée correctement après backup

\- \[ ] Manifest Ludusavi caché, re-téléchargement uniquement si ETag changé

\- \[ ] Échec gracieux si offline + pas de cache (template `steam-saves` désactivé avec warning, le reste continue)

\- \[ ] README : install, config minimale, \*\*limitations explicites\*\* (pas de VSS, fichiers verrouillés ignorés, lancer la nuit recommandé)



\## Contraintes de qualité



\- Chemins Windows : gérer UNC (`\\\\server\\share\\…`) et espaces correctement

\- `anyhow::Result` au top-level, `thiserror` pour erreurs typées des modules

\- Pas d'`unwrap()` en prod (sauf justifié + commenté)

\- Un span tracing par job, événements début/fin/erreur

\- Pas de panique sur fichier verrouillé : log warning, continue le job



\## Instruction d'exécution pour Claude Code



Procède étape par étape (1 à 9). À la fin de chaque étape, fais un point bref : ce qui a été fait, ce qui a été décidé en cours de route (notamment toute découverte sur l'API `rustic\_core` qui pourrait remettre en cause un choix d'archi), et attends ma validation avant de passer à la suivante. Pas d'enchaînement automatique.

