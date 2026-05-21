# Kovre — Phase 6 : Restore UI

## Contexte produit

Phase 5 a livré la **vue d'inventaire** : l'utilisateur voit en une page ce qui est protégé sur sa machine. Le cycle de confiance n'est pas complet pour autant — il manque le **retour en arrière**. Aujourd'hui, restaurer un fichier passe par :
- **mirror** : ouvrir Explorer, naviguer dans le NAS, copier-coller. OK pour un fichier, hostile pour 50 GB.
- **rustic** : ligne de commande `rustic restore latest:/ /tmp/restore` avec passphrase manuelle.

Aucun chemin via le dashboard, alors que la fondation existe : `BackupEngine::restore_latest(job_name, dest_dir)` est implémentée pour les deux backends et **validée e2e** par `restore_round_trip_{rustic,mirror}` (cf. `kovre/tests/integration.rs`). Il reste à exposer ça en HTTP + UI.

Phase 6 boucle la promesse : *"voir ce qui est protégé → savoir qu'on peut le récupérer"*.

**Philosophie héritée :**
- Pas de nouvelle dépendance, pas de scope creep.
- Le `kovre.yaml` reste source de vérité ; restore ne le touche jamais.
- L'utilisateur valide où il restaure (pas d'écriture cachée).

## Décisions cadre

| Sujet | Décision | Pourquoi |
|---|---|---|
| **Mode d'exécution** | Async + poll, calque sur `JobRun` (nouveau model `RestoreRun`) | Un restore de 50–100 GB tient en plusieurs minutes — figer l'UI tout ce temps est inacceptable. On copie le pattern backup : POST retourne 202 + `id`, le frontend poll `GET /api/restore_runs/:id` jusqu'à status terminal. |
| **Picker snapshots/versions** | Hors scope Phase 6 — uniquement `restore_latest` | Pour mirror, "latest" = état canonique (pas de version archivée picker fichier-par-fichier dans la même PR). Pour rustic, "latest" = snapshot le plus récent du job. Picker = Phase 7. |
| **Validation dest_dir** | Refus des `..` (path traversal) + dest_dir doit être un répertoire ou pouvoir être créé | Identique au pattern de `init-password`. Pas de whitelist (l'utilisateur écrit où il veut sur ses propres disques). |
| **Authentification** | Hors scope Phase 6 — restore reste sur `127.0.0.1` par défaut comme le reste | L'auth `--bind 0.0.0.0` est un chantier séparé (Phase 7 release-ready). En attendant, README **durci** : `--bind 0.0.0.0` est dangereux, ne pas l'utiliser sans reverse proxy authentifié. |

## Stack additions

- **Aucune nouvelle dep** Rust.
- **Aucune nouvelle dep** frontend.

## Scope Phase 6

### Inclus

**1. Model `RestoreRun` + endpoint `POST /api/jobs/:name/restore` (backend) :**
- Nouveau model Lithair `RestoreRun` dans `kovre/src/serve/models.rs` (calque sur `JobRun`) : `id`, `job_name`, `dest_dir`, `started_at`, `finished_at: Option`, `status: "running"|"success"|"failed"`, `failure_reason: Option`, `trigger: "dashboard"|"cli"`. Enregistré dans Lithair → routes `/api/restore_runs[/:id]` auto-générées.
- Nouveau module `kovre/src/serve/restore.rs` (calque sur `runs.rs`) : `register_restore_run`, `mark_restore_success`, `mark_restore_failure`, `trigger_restore`.
- Route `POST /api/jobs/:name/restore` :
  - Body JSON : `{ dest_dir: string }`.
  - Réponse 202 : `{ id, status: "running" }`. La tâche tourne en `tokio::spawn` + `spawn_blocking` (rustic peut être long), met à jour le model en fin d'exécution.
  - Erreurs avant spawn : 400 (validation dest_dir, body absent, path traversal), 404 (job inconnu).
- Validation `dest_dir` : refus `..` dans n'importe quel segment ; création du dossier si manquant.
- Tests unit : extracteur, validation dest_dir, register_restore_run idempotency (one running restore per job), data fn.

**2. Bouton "Restore" sur l'inventaire (frontend) :**
- 4e bouton sur chaque card de `/+page.svelte` (après ▶ Run, ✎ edit, × delete) — icône `↻` ou `⏪`.
- Au clic : navigation vers `/jobs/:name/restore`.

**3. Page `/jobs/:name/restore` (frontend) :**
- Header rappelant le job (nom, repo, backend, paths sources).
- Champ `dest_dir` avec `<DirInput>` (autocomplete dossiers via `/api/fs`).
- Pré-rempli avec une suggestion sensée : `<userprofile>\kovre-restore\<job-name>\<date>` — l'utilisateur peut éditer.
- Note : "Le restore copie les fichiers sauvegardés vers `<dest_dir>`. Les fichiers existants à cet emplacement seront écrasés. Les sources d'origine ne sont jamais touchées."
- Bouton "Restore" → POST `/api/jobs/:name/restore` (202 + `id`), puis poll `GET /api/restore_runs/:id` toutes les 2 s (même pattern que backup). Pendant le poll : barre de progression indéterminée + statut courant ("copying files…"). À la fin : message succès vert ("Restored X files to <dest>") ou erreur rouge avec la `failure_reason`.
- Lien "← back to inventory" pour annuler avant lancement ; pendant un restore en cours, l'utilisateur peut quitter la page et revenir (le poll reprend via `GET /api/restore_runs/:id`).

**4. README "Restore" reformulé :**
- Section Restore mise à jour pour pointer vers le dashboard comme chemin canonique. La méthode CLI (`rustic restore latest:/`) reste documentée pour les power-users mais devient secondaire.
- Note durcie sur `--bind 0.0.0.0` : la route restore peut écrire arbitrairement sur disque, donc danger en LAN sans auth (sera traité en Phase 7).

**5. Tests e2e :**
- Étendre `dashboard_end_to_end` avec un cycle complet : créer un job mirror → run → restore vers un dest temp → diff de l'arbo restaurée vs source.
- Ajoute la couverture du handler restore au e2e existant (sans en multiplier les ServeProcess).

### Exclus

- **Picker snapshots rustic** → Phase 7.
- **Picker versions mirror par fichier** (`.versions/<rel>/<stem>-<ts>.<ext>`) → Phase 7.
- **Auth bearer-token `--bind 0.0.0.0`** → Phase 7 release-ready.
- **CI GitHub Actions + packaging** → Phase 7 release-ready.

## Definition of Done

- Un job mirror configuré dans `kovre.yaml` peut être restauré depuis le dashboard en 3 clics : home → carte → "Restore" → confirme dest → reçoit "✓ restored".
- Idem pour un job rustic (avec passphrase chargée automatiquement depuis `password_file`).
- Le test `dashboard_end_to_end` couvre le round-trip restore via API.
- README et `kovre.example.yaml` ne renvoient plus l'utilisateur à `rustic restore` pour le cas nominal.
- `cargo test --workspace --exclude kovre-wasm` reste vert (~160 tests).
- `npm --prefix web run check` reste à 0 erreur.
- Aucun nouveau warning compile.

## Étapes (workflow étape par étape — validation utilisateur entre chaque)

1. **Backend model + module** : `RestoreRun` model dans `models.rs`, module `restore.rs` (register/mark_success/mark_failure/trigger). Tests unit pour les helpers + register concurrency (refus d'un 2e restore en cours sur le même job — comme backup).
2. **Backend route** : `POST /api/jobs/:name/restore` (validation + 202 + spawn). Tests unit handler.
3. **Frontend page** : créer `/jobs/[name]/restore/+page.svelte` (form + submit + polling boucle), ajouter `restoreJob(name, dest)` et `getRestoreRun(id)` à `web/src/lib/api.ts`.
4. **Bouton inventaire** : ajouter le 4e bouton sur les cards de `/+page.svelte`.
5. **Test e2e** : étendre `dashboard_end_to_end` (mirror job → run → restore → poll → diff).
6. **Doc** : README "Restore" mis à jour + durcir la note `--bind 0.0.0.0` en attendant Phase 7.

## Contraintes de qualité

- **Pas de breaking change** sur les routes existantes ni sur le YAML.
- **Pas de path traversal** dans `dest_dir` (couvert par tests).
- **Restore est non-destructif côté source** : ne touche jamais aux paths source ni au repository d'origine ; copie uniquement vers `dest_dir`.
- **Live reload conservé** : aucune ré-initialisation du serveur n'est requise après une PUT /api/config.
- **Sécurité hot-fix README** : doit poser noir-sur-blanc que `--bind 0.0.0.0` sans auth devant expose `POST /api/jobs/:name/restore` (= lecture arbitraire du repo + écriture arbitraire sur disque). L'auth viendra en Phase 7 ; en attendant le README est l'avertissement.

Si un bug Lithair bloque une étape : remonter une issue dans `lithair/lithair`, ajouter une ligne dans `ISSUES_LITHAIR.md`. Sinon on attend le fix ou workaround temporaire `// TODO(lithair#NN): retirer ce workaround`.
