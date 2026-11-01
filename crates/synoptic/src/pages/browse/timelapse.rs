//! Time Lapse page: `TimeLapse.Recording.List` rows shaped like Recordings
//! with a Locked chip and Play / Download / Lock–Unlock / Delete (Delete is
//! disabled while locked). Unpaginated. `ss_api` has the models but no
//! calls for this API, so the requests are raw `ApiRequest`s mirroring the
//! reference client's `services/timelapse.py`.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use serde::Deserialize;
use ss_api::{ApiRequest, Client, Recording, TimeLapseRecording, TimeLapseTask};

use super::recordings::{fetch_thumbnail_b64, show_thumb, texture_from_b64, thumb_slot};
use super::{
    Browse, Content, DownloadKind, clear_box, format_duration, format_range, format_size, hint,
    media_filename, parent_window, start_download,
};
use crate::icons;
use crate::widgets::controls::{label, set_class};

const API_TASK: &str = "SYNO.SurveillanceStation.TimeLapse";
const API_REC: &str = "SYNO.SurveillanceStation.TimeLapse.Recording";

/// `TimeLapse.ListTask` v1 → tasks (empty on any failure).
async fn list_tasks(client: &Client) -> Vec<TimeLapseTask> {
    let req = ApiRequest::new(API_TASK, "ListTask", 1);
    let Ok(data) = client.send(&req).await else {
        return Vec::new();
    };
    data.get("task")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| TimeLapseTask::deserialize(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// `TimeLapse.Recording.List` v1 for every task in `[from, to]`.
async fn list_recordings(
    client: &Client,
    range: Option<(i64, i64)>,
) -> ss_api::Result<Vec<TimeLapseRecording>> {
    let mut req = ApiRequest::new(API_REC, "List", 1)
        .param("lapseId", -1)
        .param("start", 0)
        .param("limit", 100)
        .param("locked", 0)
        .param("blIncludeSnapshot", "false");
    if let Some((from, to)) = range {
        req = req.param("fromTime", from).param("toTime", to);
    }
    let data = client.send(&req).await?;
    Ok(data
        .get("events")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| TimeLapseRecording::deserialize(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default())
}

/// `Lock` / `Unlock` / `Delete` with `idList`.
async fn recording_action(client: &Client, method: &str, id: i64) -> ss_api::Result<()> {
    let req = ApiRequest::new(API_REC, method, 1).param("idList", id);
    client.send(&req).await.map(drop)
}

/// The time lapse as a [`Recording`] for the player (`recEvtType` 3).
pub fn to_recording(rec: &TimeLapseRecording) -> Recording {
    Recording {
        id: rec.id,
        camera_id: rec.camera_id,
        camera_name: rec.camera_name.clone(),
        start_time: rec.start_time,
        stop_time: rec.stop_time,
        file_size: rec.file_size,
        event_type: 3,
        mount_id: rec.mount_id,
        arch_id: rec.arch_id,
        detection_label: 0,
    }
}

/// Row title: the task name for the camera, else the camera name.
pub fn title_for(rec: &TimeLapseRecording, tasks: &[TimeLapseTask]) -> String {
    tasks
        .iter()
        .find(|t| t.camera_id == rec.camera_id && !t.name.is_empty())
        .map(|t| t.name.clone())
        .unwrap_or_else(|| {
            if rec.camera_name.is_empty() {
                format!("Camera {}", rec.camera_id)
            } else {
                rec.camera_name.clone()
            }
        })
}

fn play(ctx: &Rc<Browse>, anchor: &gtk::Widget, rec: &TimeLapseRecording) {
    let Some(window) = parent_window(anchor) else {
        return;
    };
    crate::dialogs::player::open(&window, &ctx.store, &ctx.services, to_recording(rec), None);
}

fn action(ctx: &Rc<Browse>, method: &'static str, id: i64, done: &'static str) {
    let Some(client) = ctx.client() else {
        ctx.toast("Not connected");
        return;
    };
    let fut = ctx
        .services
        .run(async move { recording_action(&client, method, id).await });
    let ctx = ctx.clone();
    glib::spawn_future_local(async move {
        match fut.await {
            Ok(()) => {
                ctx.toast(done);
                ctx.refresh();
            }
            Err(e) => ctx.toast(format!("{method} failed: {e}")),
        }
    });
}

fn row(ctx: &Rc<Browse>, rec: &TimeLapseRecording, tasks: &[TimeLapseTask]) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    row.add_css_class("list-row");
    row.add_css_class("clickable");

    let duration = (rec.stop_time - rec.start_time).max(0);
    let (thumb, picture) = thumb_slot(&format_duration(duration));
    row.append(&thumb);

    let mid = gtk::Box::new(gtk::Orientation::Vertical, 4);
    mid.set_hexpand(true);
    mid.set_valign(gtk::Align::Center);
    let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = title_for(rec, tasks);
    let title_label = label(&title, &["row-title"]);
    title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title_row.append(&title_label);
    if rec.is_locked {
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        chip.add_css_class("detect-chip");
        chip.add_css_class("locked-chip");
        chip.set_valign(gtk::Align::Center);
        chip.append(&icons::image("lock", 12));
        chip.append(&gtk::Label::new(Some("Locked")));
        title_row.append(&chip);
    }
    mid.append(&title_row);
    mid.append(&label(
        &format_range(rec.start_time, rec.stop_time, " → "),
        &["row-range"],
    ));
    let mut meta = vec![format_size(rec.file_size), format_duration(duration)];
    if !rec.camera_name.is_empty() && rec.camera_name != title {
        meta.push(rec.camera_name.clone());
    }
    if rec.recording {
        meta.push("recording".into());
    }
    mid.append(&label(&meta.join(" · "), &["row-meta"]));
    row.append(&mid);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.add_css_class("row-actions");
    actions.set_valign(gtk::Align::Center);

    let play_btn = icons::bordered_button("play_arrow-fill", "icon-34", "Play");
    {
        let ctx = ctx.clone();
        let rec = rec.clone();
        play_btn.connect_clicked(move |b| play(&ctx, b.upcast_ref(), &rec));
    }
    actions.append(&play_btn);

    let dl_btn = icons::bordered_button("download", "icon-34", "Download");
    {
        let ctx = ctx.clone();
        let rec = rec.clone();
        let title = title.clone();
        dl_btn.connect_clicked(move |_| {
            start_download(
                &ctx.store,
                &ctx.services,
                DownloadKind::TimeLapse(rec.id),
                media_filename(&title, rec.start_time, "mp4"),
            );
        });
    }
    actions.append(&dl_btn);

    let (lock_icon, lock_tip) = if rec.is_locked {
        ("lock", "Unlock")
    } else {
        ("lock_open", "Lock")
    };
    let lock_btn = icons::bordered_button(lock_icon, "icon-34", lock_tip);
    set_class(&lock_btn, "locked", rec.is_locked);
    {
        let ctx = ctx.clone();
        let (id, locked) = (rec.id, rec.is_locked);
        lock_btn.connect_clicked(move |_| {
            if locked {
                action(&ctx, "Unlock", id, "Recording unlocked");
            } else {
                action(&ctx, "Lock", id, "Recording locked");
            }
        });
    }
    actions.append(&lock_btn);

    let del_btn = icons::bordered_button("delete", "icon-34", "Delete");
    del_btn.add_css_class("danger");
    del_btn.set_sensitive(!rec.is_locked);
    {
        let ctx = ctx.clone();
        let id = rec.id;
        del_btn.connect_clicked(move |b| {
            b.set_sensitive(false);
            action(&ctx, "Delete", id, "Time lapse recording deleted");
        });
    }
    actions.append(&del_btn);
    row.append(&actions);

    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    {
        let ctx = ctx.clone();
        let rec = rec.clone();
        let row_w = row.clone();
        click.connect_released(move |g, _, x, y| {
            if row_w.contains(x, y) {
                g.set_state(gtk::EventSequenceState::Claimed);
                play(&ctx, row_w.upcast_ref(), &rec);
            }
        });
    }
    row.add_controller(click);

    if let Some(client) = ctx.client() {
        let fut = ctx.services.run(fetch_thumbnail_b64(
            client,
            rec.camera_id,
            rec.arch_id,
            rec.mount_id,
            rec.start_time,
        ));
        glib::spawn_future_local(async move {
            if let Some(b64) = fut.await
                && let Some(tex) = texture_from_b64(&b64)
            {
                show_thumb(&picture, tex);
            }
        });
    }

    row.upcast()
}

pub fn build(ctx: &Rc<Browse>) -> Content {
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.add_css_class("browse-list");
    scroller.set_child(Some(&list));

    let load: Rc<dyn Fn()> = {
        let ctx = ctx.clone();
        let list = list.clone();
        Rc::new(move || {
            let generation = ctx.generation();
            ctx.take_force();
            clear_box(&list);
            let Some(client) = ctx.client() else {
                list.append(&hint(
                    "Not connected. Time lapse recordings appear after login.",
                ));
                ctx.loaded(0);
                return;
            };
            list.append(&hint("Loading time lapse recordings…"));
            let range = ctx.time_range_unix();
            let camera = ctx.filter.borrow().camera;
            let fut = ctx.services.run(async move {
                let tasks = list_tasks(&client).await;
                list_recordings(&client, range)
                    .await
                    .map(|recs| (tasks, recs))
            });
            let ctx = ctx.clone();
            let list = list.clone();
            glib::spawn_future_local(async move {
                let result = fut.await;
                if !ctx.is_current(generation) {
                    return;
                }
                clear_box(&list);
                match result {
                    Ok((tasks, recs)) => {
                        let recs: Vec<&TimeLapseRecording> = recs
                            .iter()
                            .filter(|r| camera.is_none_or(|c| r.camera_id == c))
                            .collect();
                        if recs.is_empty() {
                            list.append(&hint(
                                "No time lapse recordings match the current filter.",
                            ));
                        }
                        for rec in &recs {
                            list.append(&row(&ctx, rec, &tasks));
                        }
                        ctx.loaded(recs.len() as i64);
                    }
                    Err(e) => {
                        list.append(&hint(&format!("Could not load time lapse recordings: {e}")));
                        ctx.loaded(0);
                    }
                }
            });
        })
    };

    Content {
        widget: scroller.upcast(),
        load,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(camera_id: i64, name: &str) -> TimeLapseRecording {
        TimeLapseRecording {
            id: 7,
            camera_id,
            camera_name: name.to_string(),
            start_time: 100,
            stop_time: 160,
            file_size: 2048,
            mount_id: 0,
            arch_id: 1,
            is_locked: true,
            recording: false,
            path: String::new(),
        }
    }

    #[test]
    fn titles_and_playback_model() {
        let tasks = vec![
            TimeLapseTask {
                id: 1,
                name: "Garden build".into(),
                camera_id: 3,
                camera_name: "CAM 3".into(),
            },
            TimeLapseTask {
                id: 2,
                name: String::new(),
                camera_id: 4,
                camera_name: "CAM 4".into(),
            },
        ];
        assert_eq!(title_for(&rec(3, "CAM 3"), &tasks), "Garden build");
        assert_eq!(title_for(&rec(4, "CAM 4"), &tasks), "CAM 4");
        assert_eq!(title_for(&rec(9, ""), &tasks), "Camera 9");

        let r = to_recording(&rec(3, "CAM 3"));
        assert_eq!((r.id, r.event_type, r.arch_id), (7, 3, 1));
        assert_eq!(r.duration_secs(), 60);
    }
}
