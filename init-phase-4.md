# Kovre — Phase 4 : Multi-backend (rustic + mirror versionné)

## Contexte produit

Phases 1+2+3+3.5 ont livré un dashboard complet (édition `kovre.yaml` via UI, ajout/modif/delete jobs + repositories, live reload via `ArcSwap`, single binary 32 MB). Tout passe aujourd'hui par **rustic** — moteur de backup déduppliqué chiffré au format restic. C'est excellent pour les dev-repos, la rotation d'historique, l'intégrité par hash. C'est moins adapté pour les photos / documents personnels où l'utilisateur veut juste **browser le NAS comme un clone du dossier source** et ne pas dépendre de `rustic restore`.

Phase 4 introduit le **choix de backend par repository**. Deux moteurs supportés :

- **`rustic`** (existant) — pour les use-cases où la dedup et l'historique snapshot ont de la valeur (dev-repos, logs, bases de données dumpées). Restic-compatible, chiffré.
- **`mirror`** (nouveau) — versioned mirror pour les fichiers que l'utilisateur veut retrouver tels quels. Le NAS reflète l'arbo source 1:1 ; les versions précédentes des fichiers écrasés vivent sous `.versions/`.

**Philosophie héritée :**
- Le `kovre.yaml` reste la source de vérité. `repositories.<name>.backend: rustic|mirror` (défaut `rustic` pour compat Phase 1+2+3).
- Single binary, dashboard embarqué.
- Le moteur de backup est une abstraction interne (`trait BackupEngine`). Les routes API, le frontend, le scheduler restent agnostiques du backend.

## Stack additions

- Aucune nouvelle dep majeure côté Rust. `walkdir` est déjà dans kovre-core (utilisé par les templates). `std::fs` couvre le reste.
- Aucune nouvelle dep frontend.

## Format `mirror` — spec

Pour un repo `photos-mirror` avec `path: \\nas\photos-versions` et un job `photos` qui sauvegarde `D:\Pictures` :

```
\\nas\photos-versions\
  ├── photos\                          ← job_name = nom du sous-dossier racine
  │   ├── Pictures\                    ← arbo 1:1 du source
  │   │   ├── 2024\
  │   │   │   └── Noël\famille.jpg     ← version courante, browsable dans Explorer
  │   │   └── 2025\
  │   └── .versions\                   ← versions précédentes des fichiers écrasés / supprimés
  │       └── Pictures\2024\Noël\
  │           ├── famille-2026-05-01-1430.jpg
  │           └── famille-2026-05-13-0830.jpg
```

Règles de l'algorithme de backup pour `mirror` :

1. **Walk source** (via `walkdir`, en appliquant les excludes du job)
2. **Pour chaque fichier source** :
   - **Pas de fichier dest correspondant** → copie directe au path canonique
   - **Fichier dest existe, mtime+size identiques** → no-op (skip pour vitesse)
   - **Fichier dest existe, mtime ou size diffèrent** → DÉPLACE le dest existant vers `.versions/<relpath>/<basename>-<ts>.<ext>` puis copie le nouveau au path canonique
3. **Walk dest current** (hors `.versions/`) → tout fichier qui n'a plus son équivalent côté source → DÉPLACE vers `.versions/` (symétrique avec les écrasements)
4. **Retention** sur `.versions/` : pour chaque fichier canonique, garder au plus `keep_versions` versions, supprimer les plus anciennes

**Ce que `mirror` ne fait PAS :**
- Pas de chiffrement (les fichiers sont en clair sur le NAS).
- Pas de dedup au niveau bloc (NTFS hardlinks possibles plus tard, hors scope Phase 4).
- Pas de snapshot atomique au sens rustic (`list_snapshots` retourne une liste vide pour les repos mirror — le `JobRun` couvre l'aspect "quand est-ce que ça a tourné").
- Pas de support des fichiers verrouillés (mêmes limitations que rustic Phase 1).

## Scope Phase 4

### Inclus

**Backend / kovre-core :**
- `trait BackupEngine` avec `init`, `backup`, `list_snapshots`, `apply_retention`. Implémentations `RusticEngine` (refacto de l'existant) et `MirrorEngine` (nouveau).
- `Repository.backend: BackendKind` (enum `Rustic` / `Mirror`, défaut `Rustic` via serde). `password_file: Option<PathBuf>` (optionnel pour `mirror`).
- `Retention.keep_versions: Option<u32>` (nouveau champ, ignoré par rustic, utilisé par mirror).
- Factory `engine_for(repo: &Repository) -> Box<dyn BackupEngine>`.
- Validation côté `Config::from_str` : `backend: rustic` exige `password_file:`, `backend: mirror` exige son absence (warning soft, pas blocking).

**Dashboard backend (kovre/src/serve) :**
- `POST /api/repositories/:name/init` continue de marcher : dispatche sur `engine_for(repo).init()`.
- `GET /api/repositories/status` : pour rustic, check `<path>/config` ; pour mirror, check `<path>/` existe.
- `serve::sync::sync_snapshots` : skip les repos mirror (engine retourne liste vide).
- `serve::runs::trigger_job_run` : utilise `engine_for(repo).backup(...)` au lieu d'appel direct à `kovre_core::backup::backup_job`.

**Frontend :**
- Wizard `/repositories/new` et `/edit` : dropdown "Backend type" en premier champ (Rustic / Mirror). Conditionnement des champs : `password_file` + bouton Generate visibles seulement si Rustic.
- `/repositories` (table) : nouvelle colonne "Backend".
- Tiles `/` et page `/jobs/[name]` : un petit badge `📦 rustic` / `🪞 mirror` à côté du nom de job (récupéré via le `repository` → `backend`).
- Hint utilisateur : sur la galerie `/templates`, un encart "Quel backend choisir ?" qui résume les trade-offs.

**Tests :**
- 8-10 unit tests sur `MirrorEngine` : new file copy, file changed → version moved, file deleted → version moved, retention prune par fichier canonique, excludes respect, init idempotent.
- Extension de `tests/dashboard.rs` : un mirror job end-to-end (fixture qui modifie un fichier source entre deux runs, vérification que `.versions/` contient la version précédente).
- README Phase 4 : section "Choisir un backend" + spec mirror.

### Exclus (Phases ultérieures)

- Backend `zip` (un .zip horodaté par snapshot) → Phase 5 si demande.
- Hardlinks NTFS pour dedup OS-level entre `.versions/` entries identiques → Phase 5.
- Chiffrement optionnel pour mirror (AES sur les fichiers individuels) → Phase 5.
- Restore depuis UI (browser `.versions/`, sélectionner un fichier, restaurer à un path donné) → Phase 5 — gros morceau séparé.
- Scheduler intégré, VSS, service Windows, notifications → toujours pour après.

## Format YAML — additions Phase 4

```yaml
repositories:
  nas-rustic:
    backend: rustic                # default si omis (compat Phase 1+2+3)
    path: \\nas.local\backup\kovre
    password_file: C:\ProgramData\Kovre\nas.key

  photos-mirror:
    backend: mirror
    path: \\nas.local\photos-versions
    # pas de password_file

jobs:
  dev-repos:
    template: dev-repos
    repository: nas-rustic
    retention:
      keep_last: 30                # rustic snapshot retention

  family-photos:
    repository: photos-mirror
    paths:
      - D:\Pictures
    retention:
      keep_versions: 10            # mirror : 10 versions max par fichier dans .versions/
```

## Étapes (ordre)

Étapes séquentielles. **Validation utilisateur entre chaque** (workflow constant depuis Phase 1).

1. **`BackupEngine` trait + refactor RusticEngine** : extrait le trait, renomme `backup::init_repo` / `backup_job` / `list_snapshots_for_job` / `apply_retention` en méthodes de `RusticEngine`. Factory `engine_for(repo)`. Tests existants restent verts. Pas de nouveau format YAML encore (toute l'archi peut être faite avant d'introduire `backend:`).

2. **Schema YAML extension** : ajout du champ `backend` (enum `Rustic`/`Mirror` avec serde default `Rustic`) et `password_file: Option<PathBuf>`. Mise à jour de `RusticEngine` pour rejeter avec un message clair si `password_file` est absent. Validation côté `Config::from_str`. Mise à jour des fixtures de tests qui construisent un Repository.

3. **`MirrorEngine` impl** : nouveau module `kovre-core::backup::mirror`. `init`, `backup` (walk source + dest, copy/move/version), `list_snapshots` (retourne vide), `apply_retention` (prune `.versions/` par fichier canonique en fonction de `keep_versions`). 8-10 unit tests autour d'un TempDir fixture.

4. **Câblage côté `serve`** : `trigger_job_run`, `sync_snapshots`, `handle_init_repo`, `handle_repositories_status` passent par `engine_for(repo)`. Aucune nouvelle route. Test e2e dashboard étendu : un job mirror, vérif `.versions/` après deuxième backup.

5. **Frontend wizard + UI** : dropdown backend sur `RepoForm.svelte`, champ `password_file` conditionnel, badge backend sur les tiles et `/jobs/[name]`, encart "choisir un backend" sur `/templates`. README Phase 4 : section spec mirror + comparatif rustic vs mirror.

## Definition of Done

- [ ] `cargo build --release` produit toujours un binaire qui marche
- [ ] Tous les tests Phase 1+2+3+3.5 passent (régression zéro). 105 → ~115 tests.
- [ ] Un `kovre.yaml` Phase 3 sans champ `backend:` continue de booter (default `rustic`, password_file requis)
- [ ] Un repository déclaré `backend: mirror` sans `password_file` boote sans warning
- [ ] Un job sur un repo mirror s'exécute, copie les fichiers source au path canonique sur le NAS
- [ ] Modifier un fichier source puis re-lancer le job déplace l'ancienne version dans `.versions/<relpath>/<name>-<ts>.<ext>`
- [ ] Supprimer un fichier source puis re-lancer déplace dans `.versions/` (symétrique)
- [ ] `keep_versions=N` borne le nombre d'entrées dans `.versions/` par fichier canonique
- [ ] Le wizard `/repositories/new` cache le champ `password_file` quand backend = mirror
- [ ] Le bouton `Generate` n'apparaît pas pour mirror
- [ ] La galerie `/templates` montre un encart "rustic vs mirror" avec les trade-offs
- [ ] README mis à jour avec une section "Choisir un backend" + tableau comparatif
- [ ] `tests/dashboard.rs` couvre le cycle mirror (backup, modif source, second backup, vérif `.versions/`)

## Contraintes de qualité

- **Compat Phase 1+2+3** : un `kovre.yaml` sans `backend:` doit booter à l'identique. `backend` est défaulted à `rustic` via serde, `password_file` reste requis pour rustic.
- **MirrorEngine ne supprime jamais sans déplacer** : toute désynchronisation par rapport à la source (fichier modifié, fichier supprimé) passe par `.versions/`. La seule chose qui supprime est `apply_retention` quand `keep_versions` est dépassé.
- **Détection de changement** : mtime + size, pas de hash par défaut. Hash optionnel via une option future (hors Phase 4). Faux positifs acceptables (un fichier modifié à la milliseconde près sera détecté ; un fichier touché mais identique le sera aussi — coût : une version inutile, pas une perte).
- **Atomicité** : chaque opération fichier (copy + version-move) doit être atomique par fichier (write-to-tmp + rename quand possible). Pas d'atomicité globale entre fichiers — si le backup est interrompu au milieu, le NAS est dans un état cohérent par fichier mais le job apparait `failed` sur le `JobRun`.
- **Pas de chiffrement** : assumé. Le NAS doit être considéré confidentiel par les ACL OS.
- **`.versions/` est un dossier réservé** : si l'utilisateur a un sous-dossier source nommé `.versions`, on refuse l'upload avec un message d'erreur clair.
- **Path safety** : tous les paths côté dest sont relatifs au `repository.path` ; jamais d'écriture en dehors. Validation par `std::path::Path::strip_prefix` après canonicalisation.

## Instruction d'exécution pour Claude Code

Procède étape par étape (1 à 5). À la fin de chaque étape, fais un point bref : ce qui a été fait, ce qui a été décidé en cours de route (notamment toute découverte sur le comportement Windows NTFS ou rustic_core qui pourrait remettre en cause un choix), et attends la validation user avant de passer à la suivante. Pas d'enchaînement automatique.

Si un bug Lithair bloque une étape : remonter une issue dans `lithair/lithair`, ajouter une ligne dans `ISSUES_LITHAIR.md`, demander à l'utilisateur s'il préfère attendre le fix ou workaround.
