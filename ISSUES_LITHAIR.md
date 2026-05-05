# Issues rencontrées sur `lithair-core` pendant l'intégration kovre

Notes prises pendant la Phase 2, reportées en upstream sur
[lithair/lithair](https://github.com/lithair/lithair/issues). Même pattern
que `ISSUES_RUSTIC.md` côté Phase 1.

| # | Titre court | Upstream | Statut |
|---|-------------|----------|--------|
| 1 | `LithairServer` doesn't expose `/health`/`/ready`/`/info` | [lithair/lithair#40](https://github.com/lithair/lithair/issues/40) | ✅ fixed in `v0.2.0` |

## 1. `LithairServer` doesn't expose `/health`, `/ready`, `/info`

**Phase 2 step:** 2 (sous-commande `serve` squelette)
**Détecté contre :** `lithair-core = "0.1.3"`
**Fixé dans :** `lithair-core = "0.2.0"` — DeclarativeServer retiré, LithairServer devient le seul chemin et hérite des endpoints système.

Le README de Lithair annonçait *"Every Lithair server comes with /health, /ready, /info out of the box"* mais ces endpoints vivaient en réalité dans le builder legacy `DeclarativeServer`, pas dans `LithairServer` (la voie recommandée). Notre smoke test sur `serve` retournait donc 404 pour ces paths.

Résolu upstream en deux temps : (a) migration des handlers vers `LithairServer`, (b) suppression complète de `DeclarativeServer` (cf. PRs #43→#46 sur Lithair). v0.2.0 = breaking change documenté dans le tag.

Aucun workaround dans kovre — on a attendu le release.
