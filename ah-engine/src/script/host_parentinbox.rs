//! Host primitives of the `devswarm-parent-inbox` check for a Primary that runs inside a git checkout (roadmap #68 part 2B).
//! The script (`engine/logic/devswarm-parent-inbox.js`) holds every decision and text; these functions hold no rule, threshold or
//! message, they only extract what it asks for.
//!
//! | raw function | what it does |
//! |---|---|
//! | `piAppDb()` | whether the DevSwarm app's database file exists at the path the app uses (`appDbPath`): the one place the hook's wake-coverage line would look for a live child that no descriptor names |
use super::host::with_settings;
use crate::meshw::ident;
use rquickjs::{Ctx, Function, Object};
use std::path::Path;

fn app_db_present(home: &str, env: &ident::Env) -> bool {
    ident::app_db_path(Path::new(home), env).is_some_and(|p| std::fs::symlink_metadata(p).is_ok())
}

pub(super) fn install<'js>(c: &Ctx<'js>, h: &Object<'js>) -> rquickjs::Result<()> {
    h.set(
        "piAppDb",
        Function::new(c.clone(), || -> rquickjs::Result<bool> {
            with_settings(|st| app_db_present(&st.home, &st.env))
        })?,
    )?;
    Ok(())
}
