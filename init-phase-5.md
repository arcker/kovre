# Kovre — Phase 5 : Pivot inventaire

## Contexte produit

Phases 1+2+3+4 ont livré un dashboard complet avec deux backends (rustic + mirror), tests e2e backup→restore→diff, verify endpoint, multi-backend dans l'UI. **Le code est solide ; la vision est trahie.**

Un audit fresh externe (sub-agent, 2026-05-15) a constaté :

- `templates/documents.rs` = 3 dirs hardcoded. Aucun template pour mails, navigateurs, profils utilisateur, AppData "important".
- `web/src/routes/+page.svelte` est un *job runner* (tiles "Run now"), pas un *inventaire*. La promesse "en une vue je sais ce que je perds" n'est nulle part livrée.
- Le mirror détecte les changements via `mtime+size` : un fichier renommé/déplacé apparaît comme `delete + add` dans `.versions/` → désastre sur une réorganisation de photos.
- kovre est de facto positionné comme "Kopia simplifié + dev-repos auto", pas comme "inventaire de ce qui compte".

Si l'agent fresh a "mal vu" la vision, c'est que l'exécution la trahit. Phase 5 corrige ça **avant** d'empiler de nouvelles features (restore UI, scheduler).

**Philosophie héritée :**
- Le `kovre.yaml` reste source de vérité. Pas de stockage d'état parallèle.
- Single binary, dashboard embarqué, Windows-only natif.
- Rustic et mirror restent supportés tous les deux ; **mirror devient le défaut narratif** (wizard, README, badges UI), rustic reste accessible pour les use cases dev/logs.

## Décisions cadre (2026-05-15)

| Sujet | Décision | Pourquoi |
|---|---|---|
| **Sort rustic** | Gardé, dé-priorisé narrativement (wizard mirror-first, README repositionne) | Reste utile pour usage perso dev/logs/dumps ; pas de breaking change. |
| **Restore UI** | Décalé Phase 6 | Le restore est déjà validé par les e2e tests (`restore_round_trip_*`). UX viendra plus tard, peut-être en s'appuyant sur la vue inventaire ("clic fichier → restore"). |
| **Scheduler** | Hors Phase 5 | L'utilisateur garde `schtasks` (documenté README). Pas de service Windows. À reconsidérer après pivot. |

## Stack additions

- **Aucune nouvelle dep** Rust prévue.
- Templates mails/navigateurs : utilisation de `dirs::data_local_dir()` + `winreg` (déjà utilisés). Lecture des profiles Firefox via parsing de `profiles.ini` (texte simple).
- Hash mirror : `sha2 = "0.10"` (déjà transitif via rustic_core) ou direct via `std::fs::read` + `blake3` si on veut plus rapide. À trancher en étape 3.
- Frontend : aucune nouvelle dep.

## Scope Phase 5

### Inclus

**1. Endpoint résolution templates (backend) :**
- `POST /api/templates/:name/resolve` qui prend `template_options` en body et retourne `{paths: [...], excludes: [...], status: "ok"|"empty", note?: string}`. Wrap `templates::resolve_job` qui existe déjà.
- Tests handler + extracteur de nom.

**2. Templates étendus (backend) :**
- **`thunderbird-mail`** — résolu via `%APPDATA%\Thunderbird\Profiles\*.default-release\` (lecture de `profiles.ini` côté kovre-core).
- **`outlook-mail`** — résolu via `%LOCALAPPDATA%\Microsoft\Outlook\` (PST/OST) + `%APPDATA%\Microsoft\Outlook\` (signatures, règles).
- **`browser-profiles`** — résolu via `%APPDATA%\Mozilla\Firefox\Profiles\*` + `%LOCALAPPDATA%\Google\Chrome\User Data\Default\` (Bookmarks + History + Logins).
- **`user-appdata`** — un template "catch most" qui prend `%APPDATA%` filtré (exclusion `Local\Microsoft\Windows`, `Local\Temp`, etc.) — l'utilisateur peut le configurer avec une liste d'apps autorisées.
- Tests unitaires par template (TempDir + fixtures de chemins).

**3. Rename detection mirror (kovre-core) :**
- Étape "deuxième passe" dans `MirrorEngine::backup` : avant d'archiver un fichier dest "manquant" en source, calculer son hash et vérifier si un fichier source "nouveau" a le même hash. Si oui → `std::fs::rename` au lieu de archive+copy.
- Algorithme : index `(hash → dest_path)` des dest-only files, puis pour chaque source-only file, hash et lookup. Si match → move sur place.
- Cap : pas de hash si fichier > N MB (config) pour éviter le coût sur gros fichiers (vidéos 8K). Trade-off documenté.
- Tests : rename simple (photo déplacée dans un sous-dossier), rename + modification (doit fallback sur archive+copy), rename de gros fichier (skip).

**4. Vue d'inventaire (frontend, le cœur du pivot) :**
- `+page.svelte` refondé. Plus de tiles "Run now" en home — ça part dans `/jobs`.
- Nouvelle structure :
  - **Header "Ma machine"** — nom hostname, date dernier backup global, count fichiers protégés.
  - **Sections par catégorie** : "Documents", "Mails", "Navigateurs", "Jeux (saves)", "Dev", "Autres" — chacune affichant les jobs configurés + leurs paths résolus (via le nouvel endpoint).
  - **Catégories non couvertes** — un panneau "Tu ne sauvegardes pas encore : [navigateurs] [mails Thunderbird] [jeux Steam] — [+ Activer]". Ces suggestions n'apparaissent que si le template ne matche pas déjà un job existant.
  - **État de santé** par section — ✓ vert si dernier run récent (<7j), ⚠ orange si vieux, ✗ rouge si jamais ou en échec.
- `/jobs` (existante) reste accessible pour la vue opérationnelle "courte" (jobs + runs récents).

**5. Repositionnement narratif (frontend + README) :**
- Wizard `/repositories/new` : dropdown backend ordonné `mirror` (défaut) puis `rustic` (au lieu de l'inverse aujourd'hui). Hint mirror = "recommandé pour photos, docs, mails, jeux". Hint rustic = "pour dev/logs/dumps".
- README : section "Backends" reformulée. Mirror présenté en premier. Rustic positionné comme "pour les cas où dedup et historique snapshot ont de la valeur".
- Pas de migration forcée — les configs rustic existantes continuent de marcher sans toucher au YAML.

**6. Quick wins audit (cleanup) :**
- Supprimer `temp.key` à la racine + ajouter `*.key` à `.gitignore`.
- README : 117 → 125 tests.
- 3 warnings `let (dir, ...)` dans `kovre/src/serve/mod.rs:1124,1181,1202`.
- `ISSUES_LITHAIR.md` : nettoyer la ligne 32-36 incohérente.
- `ISSUES_RUSTIC.md` : ajouter colonne Statut comme Lithair.
- Découper `dashboard_end_to_end` (666 lignes) en 4-5 `#[test]` partageant la fixture.

### Exclus

- **Restore UI** → Phase 6.
- **Scheduler intégré / service Windows** → reporté ; doc `schtasks` reste.
- **Auth bearer-token** quand `--bind 0.0.0.0` → important mais hors scope pivot. Issue ouverte, README durci en attendant.
- **VSS (fichiers verrouillés)** → toujours hors scope.
- **Cloud direct (S3, B2, SFTP)** → toujours hors scope. Le NAS local reste la cible.
- **Migration `serde_yaml` → `serde_yml`** → reporté, pas bloquant.
- **CI GitHub Actions** → reporté Phase 6 ou en parallèle si temps.

## Definition of Done

- Le `+page.svelte` montre une vue inventaire structurée par catégorie, avec paths résolus concrets et état de santé. Plus de tiles "Run now" en home.
- Au moins 4 nouveaux templates en plus des 3 existants : `thunderbird-mail`, `outlook-mail`, `browser-profiles`, `user-appdata`.
- `POST /api/templates/:name/resolve` répond avec les paths concrets pour tous les templates.
- Mirror détecte les renames via hash et fait `rename` au lieu de `archive+copy` (avec fallback si hash mismatch ou fichier trop gros).
- README repositionne mirror comme défaut narratif. Wizard repo aussi.
- Quick wins audit liquidés.
- Tests : ≥140 verts (125 actuels + nouveaux templates + endpoint resolve + rename detection mirror + e2e).
- Aucun warning compile.

## Étapes (workflow étape par étape — validation utilisateur entre chaque)

1. **Endpoint `/api/templates/:name/resolve` + tests handler.** Petit, pose la fondation pour la vue inventaire. Aucun changement UI.
2. **Templates étendus** : `thunderbird-mail`, `outlook-mail`, `browser-profiles`, `user-appdata`. Chacun avec tests TempDir. Endpoint résolve les sert.
3. **Vue d'inventaire** — refonte `+page.svelte`. Header, sections par catégorie, suggestions de templates non utilisés, état de santé.
4. **Rename detection mirror** — hash comparison + tests dédiés (rename simple, rename + modif, big file skip).
5. **Repositionnement narratif** — wizard mirror-first, README "Backends" reformulé, badges.
6. **Quick wins audit** — temp.key, .gitignore, README 125 tests, warnings, ISSUES files, découpe `dashboard_end_to_end`.

## Contraintes de qualité

- **Rester aligné vision** : chaque PR doit pouvoir répondre "en quoi ça aide l'utilisateur à savoir ce qu'il sauvegarde et à se sentir protégé ?". Si la réponse est floue → reporter.
- **Pas de dépendance sur l'OS au-delà de Windows** : `winreg` + `dirs` suffisent. Pas de COM, pas de WMI.
- **Hash mirror optionnel et borné** : un fichier > 512 MB skip le hash (config). Le rename detection est best-effort, pas une garantie ; la doc doit le dire.
- **Live reload conservé** : les templates étendus s'enregistrent dans `templates::registry`, accessibles via `/api/templates` sans restart.
- **Pas de breaking change YAML** : tous les `kovre.yaml` existants doivent continuer à se charger.

Si un bug Lithair bloque une étape : remonter une issue dans `lithair/lithair`, ajouter une ligne dans `ISSUES_LITHAIR.md`. Sinon, on attend le fix ou workaround temporaire `// TODO(lithair#NN): retirer ce workaround`.
