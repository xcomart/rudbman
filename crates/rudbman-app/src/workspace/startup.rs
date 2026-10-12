//! Background work initiated once the workspace is constructed.
use super::*;

impl Workspace {
    pub(super) fn start_background_work(cx: &mut Context<Self>) {
        // The start-up update check, off the UI thread: it is an HTTPS request
        // to GitHub, and nothing on screen waits for it. The tag the user may
        // have ignored is read here, on the UI thread, because the settings
        // global is only reachable from it.
        //
        // The answer opens a dialog, so it deliberately does *not* go through
        // `open_about`'s `close_overlays` route: this is the one dialog nobody
        // asked for, arriving at a moment nobody chose, and it must never take
        // the screen from something the user opened themselves — a half-typed
        // connection form above all. If anything is already up, the check simply
        // says nothing and tries again next launch.
        //
        let ignored = app_settings::current(cx).ignored_update;
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { update::check(ignored.as_deref()) })
                .await;
            let Some(release) = found else {
                return;
            };
            this.update(cx, |workspace, cx| {
                if workspace.dialog_open(cx) {
                    log::debug!("update {} announced while a dialog is open", release.tag);
                    return;
                }
                workspace.update.update(cx, |dialog, cx| {
                    dialog.open(release, cx);
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
