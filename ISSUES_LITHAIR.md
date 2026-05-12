# Issues rencontrées sur `lithair-core` pendant l'intégration kovre

Notes prises pendant la Phase 2, reportées en upstream sur
[lithair/lithair](https://github.com/lithair/lithair/issues). Même pattern
que `ISSUES_RUSTIC.md` côté Phase 1.

| # | Titre court | Upstream | Statut |
|---|-------------|----------|--------|
| 1 | `LithairServer` doesn't expose `/health`/`/ready`/`/info` | [lithair/lithair#40](https://github.com/lithair/lithair/issues/40) | ✅ fixed in `v0.2.0` |
| 2 | `response::json` requires manual `.to_string()` on `serde_json::Value` | [lithair/lithair#47](https://github.com/lithair/lithair/issues/47) | ✅ fixed in `v0.3.0` (`response::json_value`) |
| 3 | No lightweight `query::param` for single-key extraction | [lithair/lithair#48](https://github.com/lithair/lithair/issues/48) | ✅ fixed in `v0.3.0` (`query::param`) |
| 4 | `with_route` handler signature exposes hyper types directly | [lithair/lithair#59](https://github.com/lithair/lithair/issues/59) | ⏳ open |

## 1. `LithairServer` doesn't expose `/health`, `/ready`, `/info`

**Phase 2 step:** 2 (sous-commande `serve` squelette)
**Détecté contre :** `lithair-core = "0.1.3"`
**Fixé dans :** `lithair-core = "0.2.0"` — DeclarativeServer retiré, LithairServer devient le seul chemin et hérite des endpoints système.

Le README de Lithair annonçait *"Every Lithair server comes with /health, /ready, /info out of the box"* mais ces endpoints vivaient en réalité dans le builder legacy `DeclarativeServer`, pas dans `LithairServer` (la voie recommandée). Notre smoke test sur `serve` retournait donc 404 pour ces paths.

Résolu upstream en deux temps : (a) migration des handlers vers `LithairServer`, (b) suppression complète de `DeclarativeServer` (cf. PRs #43→#46 sur Lithair). v0.2.0 = breaking change documenté dans le tag.

Aucun workaround dans kovre — on a attendu le release.

## 2. `response::json` requires manual `.to_string()` on `serde_json::Value`

**Phase 3 step:** 2 (les 3 endpoints read-only)
**Détecté contre :** `lithair-core = "0.2.0"`
**Statut :** ouverte, en attente upstream

`lithair_core::app::response::json(status, body)` prend `impl Into<String>`. Construire des bodies via `serde_json::json!` (le pattern naturel) oblige à `.to_string()` à chaque call site — boilerplate + risque de passer une string non-JSON par accident (le helper met `Content-Type: application/json` quoi qu'il arrive).

**Workaround actif côté kovre :** ~~wrapper local `json_response(status, value)` qui appelle `response::json(status, value.to_string())`~~. Supprimé une fois Lithair `v0.3.0` dispo — on utilise désormais `response::json_value(status, &value)` directement aux 14 call sites.

## 3. No lightweight `query::param` for single-key extraction

**Phase 3 step:** 2 (`GET /api/fs?path=<dir>`)
**Détecté contre :** `lithair-core = "0.2.0"`
**Statut :** ouverte, en attente upstream

Lithair expose `query::parse_query_params` qui parse une query string en `QueryParams` (skip/take/sort/filters). Pour un endpoint qui veut juste *un* paramètre décodé (ex: `path` dans `/api/fs?path=<dir>`), c'est overkill et sémantiquement faux : tout ce qui n'est pas réservé tombe dans `filters` avec un `FilterOp` parsé, donc `path=>foo` serait interprété comme un `Gt` au lieu du littéral.

**Workaround actif côté kovre :** ~~`query_param(query, key) -> Option<String>` privé dans `serve/mod.rs`~~. Supprimé une fois Lithair `v0.3.0` dispo — on utilise `lithair_core::http::query::param(query, key)` directement. Pareil pour le `percent_decode` local qu'on duplicquait : remplacé par `lithair_core::http::query::percent_decode`.

## 4. `with_route` handler signature exposes hyper types directly

**Phase 3 step:** 2 (refactor post v0.3.0)
**Détecté contre :** `lithair-core = "0.3.0"`
**Statut :** ouverte, en attente upstream

`LithairServerBuilder::with_route` exige une closure `Fn(hyper::Request<Incoming>) -> Pin<Box<dyn Future<Output = Result<hyper::Response<Full<Bytes>>>> + Send>>`. Conséquence : tout consumer doit ajouter `bytes`, `http`, `http-body-util`, `hyper` à son `Cargo.toml` juste pour typer ses handlers, plus écrire `Box::pin(async move { ... })` à chaque appel.

**Workaround actif côté kovre :** on **garde** les 4 dépendances directes (`bytes`, `http`, `http-body-util`, `hyper`) et on accepte les `Box::pin` aux call sites. À nettoyer si Lithair expose `RouteRequest`/`RouteResponse` aliases ou un helper `route_async(method, path, async |req| { ... })`.
