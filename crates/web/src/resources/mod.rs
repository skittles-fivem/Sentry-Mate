//! One module per resource group of docs/ui-api.md, each with its own `routes()`, so they are built in parallel
//! without touching the same file. The guard (`crate::policy`) checks sign-in, roles, the CSRF header and cars for
//! all of them.

use crate::AppState;
use axum::Router;

pub mod battery;
pub mod certificate;
pub mod charges;
pub mod drives;
pub mod efficiency;
pub mod fleet;
pub mod imports;
pub mod logs;
pub mod mqtt;
pub mod notices;
pub mod now;
pub mod parked;
pub mod places;
pub mod settings;
pub mod software;
pub mod statistics;
pub mod system;
pub mod tesla;
pub mod teslafi;
pub mod timeline;
pub mod tokens;
pub mod travel;

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(fleet::routes())
        .merge(now::routes())
        .merge(timeline::routes())
        .merge(drives::routes())
        .merge(charges::routes())
        .merge(parked::routes())
        .merge(battery::routes())
        .merge(efficiency::routes())
        .merge(statistics::routes())
        .merge(travel::routes())
        .merge(software::routes())
        .merge(places::routes())
        .merge(logs::routes())
        .merge(settings::routes())
        .merge(mqtt::routes())
        .merge(certificate::routes())
        .merge(imports::routes())
        .merge(system::routes())
        .merge(notices::routes())
        .merge(tokens::routes())
        .merge(tesla::routes())
        .merge(teslafi::routes())
}
