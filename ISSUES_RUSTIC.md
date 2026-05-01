# Issues rencontrées sur `rustic_core` / `rustic_backend` pendant l'intégration kovre

Notes prises au fil de la Phase 1 — à reformuler en issues GitHub pour
[rustic-rs/rustic_core](https://github.com/rustic-rs/rustic_core/issues) une
fois la phase terminée.

Versions utilisées :
- `rustic_core = "0.11.0"`
- `rustic_backend = "0.6.1"`
- toolchain : rustc 1.95.0 stable (Windows 11, target `x86_64-pc-windows-msvc`)

---

## 1. README de `rustic_core` 0.11.0 obsolète (sévérité : medium, dev-friendliness)

Les exemples du `README.md` montrent l'API d'avant 0.11 :

```rust
// README actuel :
let repo_opts = RepositoryOptions::default().password("test");
let repo = Repository::new(&repo_opts, backends)?.open()?;
```

Mais en 0.11 :
- `RepositoryOptions` n'expose plus de setter `password()` (déplacé vers `CredentialOptions`)
- `Repository::open()` prend désormais `&Credentials` :
  ```rust
  let creds = Credentials::Password("test".into());
  Repository::new(&opts, &backends)?.open(&creds)?
  ```
- Idem pour `init()` qui prend maintenant `(&Credentials, &KeyOptions, &ConfigOptions)`

Un dev qui suit le README à la lettre obtient `error: no method named 'password' found for ...` et doit fouiller dans le source pour comprendre la nouvelle API. Suggestion : mettre à jour les 3 exemples ("Initializing", "Creating snapshot", "Restoring", "Checking") pour refléter l'API courante, et idéalement noter dans un changelog ou un guide de migration depuis 0.10.

---

## 2. `BackupOptions::excludes.globs` : sémantique whitelist contre-intuitive (sévérité : high, footgun)

Le champ s'appelle `excludes` mais utilise la sémantique whitelist d'`ignore::overrides::OverrideBuilder` :

> When patterns are added, a path matching the pattern is **whitelisted** (included) ; if no pattern matches, the path is **excluded**.

Conséquence : passer `globs = vec!["**/*.tmp".into()]` ne signifie PAS "exclure les `.tmp`"
mais "n'inclure QUE les `.tmp`, exclure tout le reste".

### Reproducer minimal

```rust
let mut opts = BackupOptions::default();
opts.excludes.globs = vec!["**/*.tmp".into()];
// ... open repo, indexed_ids, backup ...
// Résultat : snapshot avec 0 fichier (que des tree blobs des dossiers parents)
//           bien que la source contienne file1.txt, file2.txt, etc.
```

### Pour exclure réellement les `.tmp`

```rust
opts.excludes.globs = vec!["!**/*.tmp".into()];   // notez le `!` initial
```

### Pourquoi c'est gênant

1. **Le nom du champ ment** : `Excludes::globs` suggère « patterns à exclure », pas « patterns à inclure exclusivement ».
2. **Symptôme silencieux** : le backup ne lève aucune erreur, le snapshot est créé, mais sans aucun fichier — détection seulement en restaurant ou en lisant le snapshot avec `rustic ls`.
3. **Divergence avec restic** : restic utilise `--exclude` avec sémantique inverse (bare pattern = exclude, `!` pour ré-inclure). Les utilisateurs venant de restic tombent dans le piège.
4. **Non documenté** : le doccomment de `Excludes::globs` est `/// Glob pattern to exclude/include` sans préciser que la sémantique par défaut est *include-only*.

### Suggestions (de la moins à la plus invasive)

a. Doc : préciser explicitement la sémantique whitelist dans le doccomment, idéalement avec un exemple « pour exclure, préfixer `!` ».

b. Renommer la struct en `Filters` et le champ en `globs_whitelist`, ou ajouter un champ `globs_blacklist: Vec<String>` dont le contenu est automatiquement préfixé `!` côté `as_override()`.

c. Inverser la sémantique pour matcher le nom : bare = exclude, `!pattern` = include. Breaking change, mais beaucoup plus sûr long terme.

Workaround côté kovre : on préfixe automatiquement `!` à tous les patterns que l'utilisateur passe via le YAML `excludes:` (cf. `src/backup.rs`). Ça nous donne la bonne sémantique pour les utilisateurs mais c'est une contournement — le bug reste exploitable par tout consommateur de l'API.

---

---

## 3. `PathList::sanitize()` est fail-all-or-nothing sur les paths inexistants (sévérité : low, ergonomie)

`PathList::sanitize()` appelle `canonicalize()` sur chaque path. Si UN seul path n'existe pas, la fonction entière échoue avec `Os { code: 123, kind: InvalidFilename }` et abandonne tous les autres paths du backup.

C'est gênant pour les use-cases où la liste de paths est générée dynamiquement et peut contenir des emplacements optionnels (templates de saves de jeux, dossiers OneDrive pas encore syncés, etc.). Le caller est obligé de pré-filtrer avec `p.exists()` avant d'appeler `sanitize`.

### Suggestion

Ajouter un mode "skip-missing" :

```rust
pub fn sanitize_skip_missing(self) -> SnapshotFileResult<(Self, Vec<PathBuf>)> {
    // Returns (sanitized_pathlist, paths_skipped_because_missing)
}
```

Ou exposer un flag `BackupOptions::skip_missing_paths: bool` qui ferait ce pré-filtrage côté `commands::backup::backup`.

Workaround côté kovre : pré-filtrage manuel sur `p.exists()` dans `backup.rs::backup_job`.

---

## 4. Format de la timestamp `SnapshotFile.time` (sévérité : low, esthétique)

`SnapshotFile::time` est un `jiff::Zoned` qui se formate par défaut avec une chaîne du genre :

```
2026-05-01T10:45:42.7376742+02:00[+02:00]
```

Le `[+02:00]` final (annotation de timezone IANA selon RFC 9557) est techniquement correct mais inattendu pour un humain qui lit la sortie d'un `list-snapshots`. La plupart des outils en présentent une version simplifiée.

Suggestion : afficher `2026-05-01 10:45:42 +02:00` ou laisser au caller le formatage, mais documenter clairement que `Display` sur `Zoned` produit cette extension RFC 9557.

(Workaround côté kovre : on pourrait re-parser le `time` et le re-formater à l'affichage. Pas urgent.)

---

(autres issues à ajouter au fil des étapes 8 → 9)
