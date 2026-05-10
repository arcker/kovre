# Kovre — Phase 3 : Édition de la config via l'UI + galerie de templates

## Contexte produit

Phase 1 a livré le moteur CLI (cf. `init.md`, terminée 2026-05-03). Phase 2 a livré le dashboard read-only sur Lithair (cf. `init-phase-2.md`, terminée 2026-05-10). En l'état, l'utilisateur doit éditer `kovre.yaml` à la main pour ajouter un job, puis redémarrer le serveur. Phase 3 ouvre cette boucle :

- Catalogue des **templates** (`documents`, `dev-repos`, `steam-saves`, `custom`) accessibles depuis le dashboard.
- Wizard guidé par template qui demande les options (`scan_root`, paths, retention) et écrit dans `kovre.yaml`.
- **Live reload** : la config est rechargée sans redémarrer `kovre serve`, le nouveau job apparaît immédiatement dans `/`.
- Picker de dossier via autocomplete serveur (FS browse côté backend, pas via l'API browser).

**Philosophie héritée :**
- Le **YAML reste l'artefact final** — éditable depuis l'UI mais aussi depuis n'importe quel éditeur de texte. Le dashboard ne devient pas la seule façon de configurer kovre.
- Édition en remplacement complet de fichier. **Les commentaires manuels sont perdus** quand l'utilisateur sauve depuis l'UI — trade-off documenté côté UI ("editing from the dashboard rewrites the file").
- AI-agent-friendly : `GET /api/config` expose le YAML brut et le parsed JSON, `PUT /api/config` accepte du YAML brut. Les agents peuvent driver kovre via REST sans browser.

## Stack additions

- **`arc-swap = "1"`** côté Rust — wrapper `ArcSwap<Config>` pour lecture wait-free + écriture atomique sans locks long-tenus.
- **`atomicwrites = "0.4"`** — pour `PUT /api/config` qui doit écrire `kovre.yaml` de façon atomique (write-to-tmp + rename, jamais un demi-fichier).
- Aucun nouveau dep frontend — formulaires Svelte 5 standards + le `kovre-wasm` existant.

## Scope Phase 3

### Inclus

**Backend :**
- `GET /api/config` — `{yaml: "<raw>", parsed: {...}}` — la config courante en deux formes
- `PUT /api/config` — accepte `{yaml: "<raw>"}`, valide via `Config::from_str`, écrit atomiquement, swap l'`ArcSwap` → handlers récupèrent la nouvelle config dès la requête suivante. Erreurs de validation : 400 + JSON détaillant la ligne/colonne fautive.
- `GET /api/templates` — liste des 4 templates (3 builtin + `custom`) avec un schéma minimal d'options (champs requis, type, description courte). Statique, hardcodé côté Rust.
- `GET /api/fs?path=<dir>` — liste les sous-dossiers de `<dir>` pour l'autocomplete. Refuse `path` non-existant, retourne 200 + liste vide pour un dossier vide. Permission Windows ACL = celle du process qui run kovre serve.

**Frontend :**
- `/templates` — galerie : 4 cartes (📄 documents, ⚙️ dev-repos, 🎮 steam-saves, 📂 custom). Click → wizard.
- Wizard inline (step-by-step ou tout sur une page selon le template) :
  - **documents** : seulement le `name` du job + retention (paths résolus par template, pas d'options à demander)
  - **dev-repos** : `name` + `scan_root` (avec autocomplete) + retention
  - **steam-saves** : `name` + retention (Steam détecté par registre, pas d'options à demander)
  - **custom** : `name` + une ou plusieurs `paths` (autocomplete chacune) + `excludes` (champ texte multilignes) + retention
- Composant `<DirInput>` réutilisable : input texte + dropdown autocomplete qui pull `/api/fs` à chaque keystroke (debounced à 200ms)
- Submit du wizard → POST patch (assemble localement le YAML, envoie en raw via `PUT /api/config`)
- Page `/config` (visualisation seule) : affiche le YAML formatté en read-only, avec un bouton "download" pour récupérer le fichier. **Pas d'édition raw** dans Phase 3 — on retient les utilisateurs hors du raw YAML pour Phase 3.

**Live reload :**
- `Arc<ArcSwap<Config>>` remplace `Arc<Config>` partout dans `serve/`
- Au démarrage : load YAML → store dans ArcSwap
- À chaque requête qui lit cfg : `cfg.load_full()` (clone d'Arc, pas de lock)
- À PUT /api/config : valider → write file atomically → `cfg.store(Arc::new(new_config))`
- Une fois swappé : la sync_snapshots / list_jobs / trigger_run récupèrent la nouvelle config dès le request suivant

### Exclus (Phase 4+)

- Édition libre du YAML brut depuis l'UI (textarea + monaco/codemirror) → Phase 4 si besoin
- Édition / suppression de jobs existants depuis l'UI → Phase 3.5 (incrément si on veut, après stabilisation Phase 3)
- Picker de dossier via FS Access API browser native → non prévu (autocomplete serveur suffit)
- Validation YAML live côté WASM → Phase 4 si désir (demande d'extraire `kovre-shared` crate sans deps Windows)
- Restore via UI → Phase 4 (gros morceau séparé)
- SSE pour logs live d'un run → Phase 4 (couplé au scheduler, mieux ensemble)
- Service Windows → toujours en dernière phase
- Notifications mail/Discord → Phase 4

## Format YAML — additions Phase 3

Aucune addition. Le format reste identique à Phase 1+2. Le wizard génère du YAML qui matche `kovre.example.yaml`.

## Étapes (ordre)

Étapes séquentielles. **Validation utilisateur entre chaque** (workflow constant depuis Phase 1).

1. **`Arc<ArcSwap<Config>>` refactor** : remplacer `Arc<Config>` partout dans `kovre/src/serve/`. Tous les tests existants doivent rester verts. Pas de nouvelle feature, juste l'infrastructure pour le live reload.

2. **`GET /api/config` + `GET /api/templates` + `GET /api/fs`** : trois endpoints read-only. Tests unitaires pour chaque. `GET /api/fs` avec validation de chemin (refuse les `..` traversals au cas où, même si on est en localhost-only).

3. **`PUT /api/config`** : endpoint d'écriture. Validation YAML via `Config::from_str` côté serveur, écriture atomique via `atomicwrites`, swap `ArcSwap`. Tests : YAML valide → 200 + cfg reloaded ; YAML invalide → 400 + structure d'erreur ; round-trip (PUT puis GET → même YAML).

4. **Page `/templates`** : galerie 4 cartes + composant `<DirInput>` autocomplete. Click sur une carte → wizard. Submit → POST `/api/config` patché → reload page d'overview. Tests Vitest sur `<DirInput>` (debounce, fetch, dropdown). Tests e2e Playwright optionnels mais probablement pas nécessaires en Phase 3.

5. **Tests d'intégration + README** : extension de `tests/dashboard.rs` avec scénario "ajouter un job documents via PUT /api/config + vérifier qu'il apparaît dans GET /api/jobs sans restart". README section Phase 3 avec captures d'écran de la galerie + wizard.

## Definition of Done

- [ ] `cargo build --release` produit toujours un binaire qui marche
- [ ] Tous les tests Phase 1+2 passent (régression zéro)
- [ ] `Arc<ArcSwap<Config>>` substitue `Arc<Config>` sans changement comportemental visible côté API
- [ ] `GET /api/config` retourne `{yaml, parsed}` reflétant le fichier sur disque
- [ ] `GET /api/templates` retourne les 4 templates avec leur schéma d'options
- [ ] `GET /api/fs?path=...` liste les sous-dossiers, refuse les paths inexistants/illégaux
- [ ] `PUT /api/config` valide, écrit atomiquement, recharge la config in-memory ; sur erreur de validation : 400 avec ligne/colonne
- [ ] `/templates` affiche les 4 cartes
- [ ] Le wizard `custom` permet d'ajouter un job avec paths via autocomplete
- [ ] Le wizard `documents` ajoute un job sans demander de paths (template les résout)
- [ ] Après submit du wizard, le nouveau job apparaît dans `/` sans redémarrer le serveur
- [ ] `/config` affiche le YAML courant en read-only + bouton download
- [ ] L'utilisateur est averti dans l'UI que sauvegarder depuis le dashboard remplace le fichier (perte des commentaires manuels)
- [ ] `tests/dashboard.rs` couvre le scénario PUT → GET sans restart
- [ ] README mis à jour avec section Phase 3 + le note sur la perte des commentaires

## Contraintes de qualité

- **Atomicité de l'écriture** : `kovre.yaml` ne doit jamais être laissé dans un état corrompu, même si le serveur crashe au milieu d'un PUT. `atomicwrites` garantit le rename atomique.
- **Validation avant écriture** : on n'écrit JAMAIS un YAML qui ne parse pas. Le `Config::from_str` est la seule barrière à respecter — pas de "best effort write then validate later".
- **Pas de locks longs sur la config** : ArcSwap, donc handlers ne bloquent jamais entre eux. Lecture wait-free.
- **`GET /api/fs` ne descend pas dans les dossiers** : retourne uniquement le contenu direct du `path` demandé. Pas de `?recursive=true`.
- **Sécurité de path** : `/api/fs` doit refuser les paths qui contiennent `..` ou des séquences de path traversal. Localhost-only ou pas, l'hygiène compte.
- **Pas de race-condition sur PUT /api/config** : si deux PUTs arrivent en même temps, le dernier gagne (ArcSwap atomic). Documenter dans la réponse que la config peut avoir changé entre la lecture et l'écriture.
- **UI feedback explicite** : la sauvegarde affiche "Saved. Job 'foo' added — visible on the overview." ou un message d'erreur structuré. Pas de flash silencieux.
- **Compat Phase 1+2** : un YAML édité à la main avant Phase 3 reste lisible. Un YAML écrit par le wizard est éditable à la main.

## Instruction d'exécution pour Claude Code

Procède étape par étape (1 à 5). À la fin de chaque étape, fais un point bref : ce qui a été fait, ce qui a été décidé en cours de route (notamment toute découverte sur l'API ArcSwap ou Lithair qui pourrait remettre en cause un choix d'archi), et attends la validation user avant de passer à la suivante. Pas d'enchaînement automatique.

Si un bug Lithair bloque une étape : remonter une issue dans `lithair/lithair`, ajouter une ligne dans `ISSUES_LITHAIR.md`, demander à l'utilisateur s'il préfère attendre le fix ou workaround.
