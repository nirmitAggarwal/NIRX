//! In-app Git dashboard: repository status, branches, commit graph, commit
//! details and a year-long contribution heatmap.
//!
//! Git itself is accessed through `crate::git`; this module only builds widgets.
//! Expensive reads run on worker threads and update the window through
//! `async_channel`, so opening a large repository never blocks the GTK loop.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use crate::git;
use crate::ui::dialogs;

/// Open the dashboard for the nearest repository containing `start`.
pub fn show(parent: &impl IsA<gtk::Window>, start: &Path) {
    match git::find_repo(start) {
        Some(root) => show_repo(parent, root),
        None => show_missing(parent, start),
    }
}

fn show_missing(parent: &impl IsA<gtk::Window>, start: &Path) {
    let win = gtk::Window::builder()
        .title("Git")
        .transient_for(parent)
        .modal(true)
        .destroy_with_parent(true)
        .default_width(460)
        .build();
    win.add_css_class("nook-dialog");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(20);
    root.set_margin_bottom(16);
    root.set_margin_start(20);
    root.set_margin_end(20);

    let title = gtk::Label::new(Some("No Git repository here"));
    title.set_xalign(0.0);
    title.add_css_class("nook-empty-title");
    let body = gtk::Label::new(Some(&format!(
        "Nook could not find a .git folder above\n{}",
        start.display()
    )));
    body.set_xalign(0.0);
    body.set_wrap(true);
    body.set_selectable(true);
    body.add_css_class("nook-empty-sub");
    root.append(&title);
    root.append(&body);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Close");
    cancel.add_css_class("nook-action");
    let init = gtk::Button::with_label("Initialize repository");
    init.add_css_class("nook-suggested");
    buttons.append(&cancel);
    buttons.append(&init);
    root.append(&buttons);

    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }
    {
        let win = win.clone();
        let parent = parent.clone();
        let start = start.to_path_buf();
        init.connect_clicked(move |_| {
            win.close();
            let parent_for_confirm = parent.clone();
            let start_for_run = start.clone();
            let parent_for_show = parent.clone();
            dialogs::confirm(
                &parent_for_confirm,
                "Initialize a repository?",
                &format!(
                    "Create a new Git repository in {}?\n\nThis only writes to that folder.",
                    start.display()
                ),
                "Initialize",
                false,
                move || match git::init(&start_for_run) {
                    Ok(_) => show(&parent_for_show, &start_for_run),
                    Err(err) => {
                        dialogs::error(&parent_for_show, "Could not initialize repository", &err)
                    }
                },
            );
        });
    }

    win.set_child(Some(&root));
    win.present();
}

/// A regular top-level window: the dashboard opens beside Nook rather than as
/// a floating dialog above it.
fn show_repo(_parent: &impl IsA<gtk::Window>, root: PathBuf) {
    let win = gtk::Window::builder()
        .title(format!("{} — Git", git::repo_name(&root)))
        .default_width(1040)
        .default_height(740)
        .build();
    win.add_css_class("nook-dialog");
    win.set_resizable(true);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    /* ------------------------------ header ------------------------------ */

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    header.add_css_class("nook-git-header");
    header.set_margin_top(12);
    header.set_margin_bottom(8);
    header.set_margin_start(16);
    header.set_margin_end(12);

    let repo_icon = gtk::Image::from_icon_name("git-symbolic");
    repo_icon.set_pixel_size(22);
    let title_box = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let repo_label = gtk::Label::new(Some(&git::repo_name(&root)));
    repo_label.set_xalign(0.0);
    repo_label.add_css_class("nook-empty-title");
    let repo_path = gtk::Label::new(Some(&dialogs::pretty_path(&root)));
    repo_path.set_xalign(0.0);
    repo_path.add_css_class("nook-meta");
    repo_path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    title_box.append(&repo_label);
    title_box.append(&repo_path);
    title_box.set_hexpand(true);

    let branch_label = gtk::Label::new(Some("…"));
    branch_label.add_css_class("nook-git-chip");
    let changes_label = gtk::Label::new(None);
    changes_label.add_css_class("nook-git-chip");
    changes_label.set_visible(false);

    let refresh = icon_button("view-refresh-symbolic", "Refresh");
    let fetch = icon_button("folder-download-symbolic", "Fetch all remotes");
    let pull = icon_button("go-down-symbolic", "Pull (fast-forward only)");
    let push = icon_button("go-up-symbolic", "Push");
    let github = icon_button("web-browser-symbolic", "Open remote on GitHub");
    github.set_sensitive(false);
    let close = icon_button("window-close-symbolic", "Close");

    header.append(&repo_icon);
    header.append(&title_box);
    header.append(&branch_label);
    header.append(&changes_label);
    header.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    header.append(&refresh);
    header.append(&fetch);
    header.append(&pull);
    header.append(&push);
    header.append(&github);
    header.append(&close);
    outer.append(&header);

    let action_status = gtk::Label::new(None);
    action_status.set_xalign(0.0);
    action_status.add_css_class("nook-meta");
    action_status.set_margin_start(16);
    action_status.set_margin_end(16);
    action_status.set_margin_bottom(6);
    action_status.set_visible(false);
    outer.append(&action_status);

    /* ------------------------------ sidebar ----------------------------- */

    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 4);
    sidebar.add_css_class("nook-git-sidebar");
    sidebar.set_margin_top(4);
    sidebar.set_margin_bottom(8);
    sidebar.set_margin_start(10);
    sidebar.set_margin_end(4);

    let branches_title = gtk::Label::new(Some("BRANCHES"));
    branches_title.set_xalign(0.0);
    branches_title.add_css_class("nook-section-label");
    sidebar.append(&branches_title);
    let branches_box = gtk::Box::new(gtk::Orientation::Vertical, 1);
    sidebar.append(&branches_box);

    let checkout = gtk::Button::with_label("Checkout selected branch");
    checkout.add_css_class("nook-action");
    checkout.set_sensitive(false);
    checkout.set_margin_top(6);
    checkout.set_margin_start(4);
    checkout.set_margin_end(4);
    sidebar.append(&checkout);

    let tree_title = gtk::Label::new(Some("WORKING TREE"));
    tree_title.set_xalign(0.0);
    tree_title.add_css_class("nook-section-label");
    sidebar.append(&tree_title);
    let tree_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    tree_box.set_margin_start(8);
    tree_box.set_margin_end(8);
    sidebar.append(&tree_box);

    let remote_title = gtk::Label::new(Some("REMOTE"));
    remote_title.set_xalign(0.0);
    remote_title.add_css_class("nook-section-label");
    sidebar.append(&remote_title);
    let remote_label = gtk::Label::new(None);
    remote_label.set_xalign(0.0);
    remote_label.set_wrap(true);
    remote_label.set_selectable(true);
    remote_label.add_css_class("nook-meta");
    remote_label.set_margin_start(8);
    remote_label.set_margin_end(8);
    sidebar.append(&remote_label);

    let sidebar_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(228)
        .max_content_width(260)
        .child(&sidebar)
        .build();

    /* ------------------------------ history ----------------------------- */

    let history_title = gtk::Label::new(Some("History"));
    history_title.set_xalign(0.0);
    history_title.add_css_class("nook-empty-title");
    history_title.set_margin_top(6);
    history_title.set_margin_bottom(4);
    history_title.set_margin_start(8);

    let history = gtk::ListBox::new();
    history.set_selection_mode(gtk::SelectionMode::Single);
    history.add_css_class("nook-git-history");

    // Commit tree: one drawing area next to the rows, so the lanes and curves
    // are painted as real lines instead of ASCII glyphs.
    let tree = Rc::new(RefCell::new(git::GraphModel::from_commits(Vec::new())));
    let graph_area = gtk::DrawingArea::new();
    graph_area.set_valign(gtk::Align::Start);
    graph_area.set_size_request(graph_width(1), -1);
    graph_area.set_hexpand(false);
    graph_area.set_vexpand(true);
    {
        let tree = tree.clone();
        let history = history.clone();
        graph_area.set_draw_func(move |_, cr, _, _| {
            draw_commit_tree(cr, &history, &tree.borrow());
        });
    }
    {
        let history = history.clone();
        let click = gtk::GestureClick::new();
        click.connect_pressed(move |_, _, _, y| {
            select_row_at(&history, y);
        });
        graph_area.add_controller(click);
    }

    let history_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    history_box.append(&graph_area);
    history_box.append(&history);
    history.set_hexpand(true);

    let history_scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&history_box)
        .build();

    let detail_title = gtk::Label::new(Some("Commit details"));
    detail_title.set_xalign(0.0);
    detail_title.add_css_class("nook-file-name");
    detail_title.set_margin_top(6);
    detail_title.set_margin_bottom(2);
    detail_title.set_margin_start(8);

    let detail_buffer = gtk::TextBuffer::new(None);
    let detail_view = gtk::TextView::with_buffer(&detail_buffer);
    detail_view.set_editable(false);
    detail_view.set_monospace(true);
    detail_view.set_wrap_mode(gtk::WrapMode::WordChar);
    detail_view.set_top_margin(6);
    detail_view.set_bottom_margin(6);
    detail_view.set_left_margin(8);
    detail_view.set_right_margin(8);
    let detail_scroller = gtk::ScrolledWindow::builder()
        .min_content_height(150)
        .child(&detail_view)
        .build();

    let details_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    details_box.append(&detail_title);
    details_box.append(&detail_scroller);

    let history_pane = gtk::Paned::new(gtk::Orientation::Vertical);
    history_pane.set_start_child(Some(&history_scroller));
    history_pane.set_end_child(Some(&details_box));
    history_pane.set_resize_start_child(true);
    history_pane.set_shrink_start_child(false);
    history_pane.set_resize_end_child(false);
    history_pane.set_shrink_end_child(true);
    history_pane.set_position(430);

    let history_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    history_frame.append(&history_title);
    history_frame.append(&history_pane);
    history_frame.set_hexpand(true);
    history_frame.set_vexpand(true);

    /* --------------------------- contributions -------------------------- */

    let contributions_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    contributions_box.set_margin_start(10);
    contributions_box.set_margin_end(10);
    contributions_box.set_margin_top(6);
    contributions_box.set_margin_bottom(8);
    let heatmap = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    contributions_box.append(&heatmap);
    let contributions_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&contributions_box)
        .build();
    let contributions_expander = gtk::Expander::new(Some("Contributions · last year"));
    contributions_expander.set_expanded(true);
    contributions_expander.set_child(Some(&contributions_scroller));
    contributions_expander.set_margin_start(8);
    contributions_expander.set_margin_end(8);
    contributions_expander.set_margin_bottom(8);

    let right = gtk::Box::new(gtk::Orientation::Vertical, 0);
    right.append(&history_frame);
    right.append(&contributions_expander);
    right.set_hexpand(true);

    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_start_child(Some(&sidebar_scroller));
    paned.set_end_child(Some(&right));
    paned.set_position(240);
    paned.set_shrink_start_child(false);
    paned.set_resize_start_child(false);
    paned.set_resize_end_child(true);
    paned.set_vexpand(true);
    outer.append(&paned);

    win.set_child(Some(&outer));

    /* ----------------------------- behaviour ---------------------------- */

    let root_for_watchers = root.clone();
    let hashes: Rc<RefCell<Vec<Option<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_branch: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let generation = Rc::new(Cell::new(0u64));
    let current_branch = Rc::new(RefCell::new(String::new()));
    let remote = Rc::new(RefCell::new(String::new()));
    let reload_holder: Rc<RefCell<Option<Rc<dyn Fn(Option<String>)>>>> =
        Rc::new(RefCell::new(None));

    {
        let detail_buffer = detail_buffer.clone();
        let hashes = hashes.clone();
        let root = root.clone();
        history.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let index = row.index();
            if index < 0 {
                return;
            }
            let Some(Some(hash)) = hashes.borrow().get(index as usize).cloned() else {
                return;
            };
            detail_buffer.set_text("Loading commit…");
            let (tx, rx) = async_channel::bounded(1);
            let root = root.clone();
            let hash_for_thread = hash.clone();
            std::thread::spawn(move || {
                let _ = tx.send_blocking(git::commit_details(&root, &hash_for_thread));
            });
            let buffer = detail_buffer.clone();
            glib::spawn_future_local(async move {
                match rx.recv().await {
                    Ok(Ok(text)) => buffer.set_text(&text),
                    Ok(Err(err)) => buffer.set_text(&format!("Could not read commit:\n{err}")),
                    Err(_) => {}
                }
            });
        });
    }

    let reload: Rc<dyn Fn(Option<String>)> = {
        let root = root.clone();
        let generation = generation.clone();
        let hashes = hashes.clone();
        let reload_holder = reload_holder.clone();
        let selected_branch = selected_branch.clone();
        let current_branch = current_branch.clone();
        let remote = remote.clone();
        let history = history.clone();
        let branches_box = branches_box.clone();
        let tree_box = tree_box.clone();
        let remote_label = remote_label.clone();
        let branch_label = branch_label.clone();
        let changes_label = changes_label.clone();
        let github = github.clone();
        let checkout = checkout.clone();
        let heatmap = heatmap.clone();
        let action_status = action_status.clone();
        let detail_buffer = detail_buffer.clone();
        let graph_area = graph_area.clone();
        let tree = tree.clone();
        Rc::new(move |filter| {
            let reload_holder = reload_holder.clone();
            let next = generation.get() + 1;
            generation.set(next);
            action_status.set_text("Loading…");
            action_status.set_visible(true);
            let (tx, rx) = async_channel::bounded(1);
            let root_for_thread = root.clone();
            let filter_for_thread = filter.clone();
            std::thread::spawn(move || {
                let _ = tx.send_blocking(git::snapshot(
                    &root_for_thread,
                    filter_for_thread.as_deref(),
                    500,
                ));
            });

            let widgets = (
                generation.clone(),
                hashes.clone(),
                selected_branch.clone(),
                current_branch.clone(),
                remote.clone(),
                history.clone(),
                branches_box.clone(),
                tree_box.clone(),
                remote_label.clone(),
                branch_label.clone(),
                changes_label.clone(),
                github.clone(),
                checkout.clone(),
                heatmap.clone(),
                action_status.clone(),
                detail_buffer.clone(),
                graph_area.clone(),
                tree.clone(),
            );
            glib::spawn_future_local(async move {
                let Ok(result) = rx.recv().await else { return };
                let (
                    generation,
                    hashes,
                    selected_branch,
                    current_branch,
                    remote,
                    history,
                    branches_box,
                    tree_box,
                    remote_label,
                    branch_label,
                    changes_label,
                    github,
                    checkout,
                    heatmap,
                    action_status,
                    detail_buffer,
                    graph_area,
                    tree,
                ) = widgets;
                if generation.get() != next {
                    return;
                }
                action_status.set_visible(false);
                match result {
                    Ok(snapshot) => {
                        *current_branch.borrow_mut() = snapshot.status.branch.clone();
                        *remote.borrow_mut() = snapshot.remote.clone();
                        branch_label.set_text(&format!("  {}  ", snapshot.status.branch));
                        if snapshot.status.changes() > 0 {
                            changes_label.set_text(&format!(
                                " {} change{} ",
                                snapshot.status.changes(),
                                if snapshot.status.changes() == 1 { "" } else { "s" }
                            ));
                            changes_label.set_visible(true);
                        } else {
                            changes_label.set_visible(false);
                        }
                        github.set_sensitive(git::github_url(&snapshot.remote).is_some());

                        // Branches -------------------------------------------------
                        while let Some(child) = branches_box.first_child() {
                            branches_box.remove(&child);
                        }
                        let active_filter = selected_branch.borrow().clone();
                        let all = branch_row(
                            "All branches",
                            None,
                            snapshot.status.branch.is_empty(),
                        );
                        if active_filter.is_none() {
                            all.add_css_class("selected");
                        }
                        {
                            let selected_branch = selected_branch.clone();
                            let detail_buffer = detail_buffer.clone();
                            let reload_holder = reload_holder.clone();
                            all.connect_clicked(move |_| {
                                *selected_branch.borrow_mut() = None;
                                detail_buffer.set_text("");
                                if let Some(reload) = reload_holder.borrow().as_ref() {
                                    reload(None);
                                }
                            });
                        }
                        branches_box.append(&all);
                        for branch in &snapshot.branches {
                            let row = branch_row(&branch.name, Some(&branch.track), branch.current);
                            if snapshot
                                .status
                                .branch
                                .eq_ignore_ascii_case(&branch.name)
                            {
                                row.add_css_class("current");
                            }
                            if active_filter.as_deref() == Some(branch.name.as_str()) {
                                row.add_css_class("selected");
                            }
                            {
                                let selected_branch = selected_branch.clone();
                                let detail_buffer = detail_buffer.clone();
                                let reload_holder = reload_holder.clone();
                                let name = branch.name.clone();
                                row.connect_clicked(move |_| {
                                    *selected_branch.borrow_mut() = Some(name.clone());
                                    detail_buffer.set_text("");
                                    if let Some(reload) = reload_holder.borrow().as_ref() {
                                        reload(Some(name.clone()));
                                    }
                                });
                            }
                            branches_box.append(&row);
                        }

                        // Working tree summary ------------------------------------
                        while let Some(child) = tree_box.first_child() {
                            tree_box.remove(&child);
                        }
                        let stat = |label: &str, value: u32, class: &str| {
                            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                            let name = gtk::Label::new(Some(label));
                            name.set_xalign(0.0);
                            name.set_hexpand(true);
                            name.add_css_class("nook-meta");
                            let count = gtk::Label::new(Some(&value.to_string()));
                            count.add_css_class("nook-git-count");
                            count.add_css_class(class);
                            row.append(&name);
                            row.append(&count);
                            row
                        };
                        tree_box.append(&stat("Staged", snapshot.status.staged, "staged"));
                        tree_box.append(&stat("Modified", snapshot.status.modified, "modified"));
                        tree_box.append(&stat("Untracked", snapshot.status.untracked, "untracked"));
                        tree_box.append(&stat("Conflicted", snapshot.status.conflicted, "conflicted"));
                        if snapshot.status.is_clean() {
                            let clean = gtk::Label::new(Some("Working tree clean"));
                            clean.set_xalign(0.0);
                            clean.add_css_class("nook-git-clean");
                            tree_box.append(&clean);
                        }
                        if snapshot.status.ahead > 0 || snapshot.status.behind > 0 {
                            let track = gtk::Label::new(Some(&format!(
                                "↑ {}  ↓ {}",
                                snapshot.status.ahead, snapshot.status.behind
                            )));
                            track.set_xalign(0.0);
                            track.add_css_class("nook-meta");
                            tree_box.append(&track);
                        }

                        remote_label.set_text(if snapshot.remote.is_empty() {
                            "No remote configured"
                        } else {
                            &snapshot.remote
                        });
                        remote_label.set_tooltip_text(Some(&snapshot.remote));

                        // History -------------------------------------------------
                        while let Some(child) = history.first_child() {
                            history.remove(&child);
                        }
                        *tree.borrow_mut() = snapshot.tree.clone();
                        let mut next_hashes = Vec::new();
                        let mut first_commit_row: Option<gtk::ListBoxRow> = None;
                        for entry in &snapshot.tree.rows {
                            let Some(commit) = entry.commit.as_ref() else {
                                continue;
                            };
                            next_hashes.push(Some(commit.hash.clone()));
                            let row = commit_row(commit);
                            if first_commit_row.is_none() {
                                first_commit_row = Some(row.clone());
                            }
                            history.append(&row);
                        }
                        *hashes.borrow_mut() = next_hashes;
                        graph_area.set_size_request(graph_width(snapshot.tree.lanes), -1);
                        graph_area.queue_draw();
                        if snapshot.tree.rows.is_empty() {
                            let row = gtk::ListBoxRow::new();
                            row.set_selectable(false);
                            let empty = gtk::Label::new(Some(
                                "No commits yet — stage a change and create the first commit.",
                            ));
                            empty.add_css_class("nook-meta");
                            empty.set_margin_top(16);
                            empty.set_margin_bottom(16);
                            empty.set_margin_start(12);
                            row.set_child(Some(&empty));
                            history.append(&row);
                        }

                        // Contributions ------------------------------------------
                        while let Some(child) = heatmap.first_child() {
                            heatmap.remove(&child);
                        }
                        heatmap.append(&contribution_heatmap(&snapshot.contributions));

                        // Checkout button follows the current branch.
                        checkout.set_sensitive(
                            !snapshot.branches.is_empty() && !snapshot.status.branch.is_empty(),
                        );
                        detail_buffer.set_text("");
                        if let Some(row) = first_commit_row {
                            history.select_row(Some(&row));
                        }
                    }
                    Err(err) => {
                        action_status.set_text(&err);
                        action_status.set_visible(true);
                    }
                }
            });
        })
    };

    *reload_holder.borrow_mut() = Some(reload.clone());

    {
        let reload = reload.clone();
        let selected_branch = selected_branch.clone();
        refresh.connect_clicked(move |_| {
            reload(selected_branch.borrow().clone());
        });
    }
    {
        let reload = reload.clone();
        let root = root.clone();
        let action_status = action_status.clone();
        fetch.connect_clicked(move |_| {
            run_action(&root, "Fetching…", git::fetch, action_status.clone(), reload.clone());
        });
    }
    {
        let reload = reload.clone();
        let root = root.clone();
        let action_status = action_status.clone();
        pull.connect_clicked(move |_| {
            run_action(&root, "Pulling…", git::pull, action_status.clone(), reload.clone());
        });
    }
    {
        let reload = reload.clone();
        let root = root.clone();
        let action_status = action_status.clone();
        let win = win.clone();
        push.connect_clicked(move |_| {
            let root = root.clone();
            let action_status = action_status.clone();
            let reload = reload.clone();
            dialogs::confirm(
                &win,
                "Push to the remote?",
                "Publish the current branch to its configured upstream.",
                "Push",
                false,
                move || {
                    run_action(&root, "Pushing…", git::push, action_status.clone(), reload.clone());
                },
            );
        });
    }
    {
        let remote = remote.clone();
        let win = win.clone();
        github.connect_clicked(move |_| {
            if let Some(url) = git::github_url(&remote.borrow()) {
                if let Err(err) = gtk::gio::AppInfo::launch_default_for_uri(
                    &url,
                    None::<&gtk::gio::AppLaunchContext>,
                ) {
                    dialogs::error(&win, "Could not open GitHub", &err.to_string());
                }
            }
        });
    }
    {
        let win = win.clone();
        close.connect_clicked(move |_| win.close());
    }
    {
        let reload = reload.clone();
        let selected_branch = selected_branch.clone();
        let current_branch = current_branch.clone();
        let win = win.clone();
        checkout.connect_clicked(move |_| {
            let Some(branch) = selected_branch.borrow().clone() else {
                return;
            };
            if branch == *current_branch.borrow() {
                return;
            }
            let root = root.clone();
            let reload = reload.clone();
            let win2 = win.clone();
            dialogs::confirm(
                &win,
                &format!("Switch to {branch}?"),
                "Uncommitted changes may prevent Git from switching branches.",
                "Checkout",
                false,
                move || {
                    let root = root.clone();
                    let reload = reload.clone();
                    let branch = branch.clone();
                    let (tx, rx) = async_channel::bounded(1);
                    std::thread::spawn(move || {
                        let _ = tx.send_blocking(git::checkout(&root, &branch));
                    });
                    let win = win2.clone();
                    glib::spawn_future_local(async move {
                        match rx.recv().await {
                            Ok(Ok(_)) => reload(None),
                            Ok(Err(err)) => dialogs::error(&win, "Checkout failed", &err),
                            Err(_) => {}
                        }
                    });
                },
            );
        });
    }

    // Live updates: watch the parts of .git that change on commit, checkout,
    // fetch and push, and refresh the graph (debounced) when any of them move.
    {
        let reload = reload.clone();
        let selected_branch = selected_branch.clone();
        let monitors = Rc::new(install_git_watchers(&root_for_watchers, move || {
            reload(selected_branch.borrow().clone());
        }));
        win.connect_destroy(move |_| {
            // Keeping the monitors in this closure ties their lifetime to the
            // window without leaking them after it is closed.
            let _ = monitors.len();
        });
    }

    reload(None);
    win.present();
}

/// Monitors for the files and directories Git touches on history changes.
fn install_git_watchers<F>(root: &Path, callback: F) -> Vec<gtk::gio::FileMonitor>
where
    F: Fn() + 'static,
{
    let pending = Rc::new(Cell::new(false));
    let callback: Rc<dyn Fn()> = Rc::new(callback);
    let mut monitors = Vec::new();
    // In a linked worktree `.git` is a file pointing at the real git dir.
    let dot_git = root.join(".git");
    let git = if dot_git.is_dir() {
        dot_git.clone()
    } else if dot_git.is_file() {
        std::fs::read_to_string(&dot_git)
            .ok()
            .and_then(|text| {
                text.lines().find_map(|line| {
                    line.strip_prefix("gitdir:")
                        .map(|value| root.join(value.trim()))
                })
            })
            .unwrap_or(dot_git)
    } else {
        dot_git
    };

    let schedule = {
        let pending = pending.clone();
        let callback = callback.clone();
        move || {
            if pending.replace(true) {
                return;
            }
            let pending = pending.clone();
            let callback = callback.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(350), move || {
                pending.set(false);
                callback();
            });
        }
    };

    for directory in [
        git.clone(),
        git.join("refs"),
        git.join("refs/heads"),
        git.join("logs"),
    ] {
        if !directory.is_dir() {
            continue;
        }
        let file = gtk::gio::File::for_path(&directory);
        let monitor = match file.monitor_directory(
            gtk::gio::FileMonitorFlags::WATCH_MOVES,
            gtk::gio::Cancellable::NONE,
        ) {
            Ok(monitor) => monitor,
            Err(_) => continue,
        };
        let schedule = schedule.clone();
        monitor.connect_changed(move |_, _, _, _| schedule());
        monitors.push(monitor);
    }

    for path in [
        git.join("HEAD"),
        git.join("index"),
        git.join("packed-refs"),
        git.join("logs/HEAD"),
    ] {
        if !path.exists() {
            continue;
        }
        let file = gtk::gio::File::for_path(&path);
        let monitor = match file.monitor_file(
            gtk::gio::FileMonitorFlags::NONE,
            gtk::gio::Cancellable::NONE,
        ) {
            Ok(monitor) => monitor,
            Err(_) => continue,
        };
        let schedule = schedule.clone();
        monitor.connect_changed(move |_, _, _, _| schedule());
        monitors.push(monitor);
    }

    monitors
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::new();
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(18);
    button.set_child(Some(&image));
    button.add_css_class("nook-btn");
    button.set_tooltip_text(Some(tooltip));
    button
}

fn branch_row(name: &str, track: Option<&str>, current: bool) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("nook-git-branch");
    button.set_has_frame(false);
    button.set_tooltip_text(Some(name));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let dot = gtk::Label::new(Some(if current { "●" } else { "○" }));
    dot.add_css_class(if current {
        "nook-git-branch-dot-current"
    } else {
        "nook-git-branch-dot"
    });
    let label = gtk::Label::new(Some(name));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    row.append(&dot);
    row.append(&label);
    if let Some(track) = track.filter(|text| !text.trim().is_empty()) {
        let badge = gtk::Label::new(Some(track));
        badge.add_css_class("nook-meta");
        row.append(&badge);
    }
    button.set_child(Some(&row));
    button
}

fn commit_row(commit: &git::Commit) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(4);
    hbox.set_margin_bottom(4);
    hbox.set_margin_start(8);
    hbox.set_margin_end(8);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 1);
    vbox.set_hexpand(true);
    let subject = gtk::Label::new(Some(&commit.subject));
    subject.set_xalign(0.0);
    subject.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subject.add_css_class("nook-git-subject");
    let meta_text = {
        let date = commit.date.get(..10).unwrap_or(&commit.date);
        let refs = if commit.refs.trim().is_empty() {
            String::new()
        } else {
            format!(" · {}", commit.refs)
        };
        format!(
            "{} · {} · {}{}",
            git::short_hash(&commit.hash),
            commit.author,
            date,
            refs
        )
    };
    let meta = gtk::Label::new(Some(&meta_text));
    meta.set_xalign(0.0);
    meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
    meta.add_css_class("nook-meta");
    vbox.append(&subject);
    vbox.append(&meta);

    hbox.append(&vbox);
    row.set_child(Some(&hbox));
    row
}

fn contribution_heatmap(counts: &BTreeMap<String, u32>) -> gtk::Widget {
    // 53 weeks x 7 days, ending today. Uses a day index derived from a civil
    // date so no extra date crate is needed.
    let grid = gtk::Grid::new();
    grid.set_row_spacing(3);
    grid.set_column_spacing(3);
    let total: u32 = counts.values().sum();
    let max = counts.values().copied().max().unwrap_or(0).max(1);
    let today = today_civil();
    let (today_year, today_month, today_day) = civil_from_days(today);
    let today_index = day_index(today_year, today_month, today_day);
    let start = today_index - (53 * 7 - 1);

    let mut header_row = 0;
    // Month labels every ~4 weeks.
    let mut last_month = 0;
    for week in 0..53 {
        let day = start + week * 7;
        let (_year, month, _day) = civil_from_days(day);
        if month != last_month && week % 4 == 0 {
            let label = gtk::Label::new(Some(month_name(month)));
            label.set_xalign(0.0);
            label.add_css_class("nook-meta");
            grid.attach(&label, week as i32, header_row, 4, 1);
            last_month = month;
        }
    }
    header_row = 1;
    for week in 0..53 {
        for weekday in 0..7 {
            let index = start + week * 7 + weekday;
            if index > today_index {
                continue;
            }
            let (year, month, day) = civil_from_days(index);
            let key = format!("{year:04}-{month:02}-{day:02}");
            let count = counts.get(&key).copied().unwrap_or(0);
            let level = if count == 0 {
                0
            } else {
                let scaled = ((count as f64 / max as f64) * 4.0).ceil() as u32;
                scaled.clamp(1, 4)
            };
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 0);
            cell.add_css_class("nook-heat");
            cell.add_css_class(&format!("nook-heat-{level}"));
            cell.set_size_request(13, 13);
            cell.set_tooltip_text(Some(&format!(
                "{count} commit{} on {key}",
                if count == 1 { "" } else { "s" }
            )));
            grid.attach(&cell, week as i32, header_row + weekday as i32, 1, 1);
        }
    }
    let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 4);
    wrapper.append(&grid);
    let legend = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let text = gtk::Label::new(Some(&format!(
        "{total} commit{} in the last year",
        if total == 1 { "" } else { "s" }
    )));
    text.add_css_class("nook-meta");
    legend.append(&text);
    wrapper.append(&legend);
    wrapper.upcast()
}

fn today_civil() -> i64 {
    let now = glib::DateTime::now_utc().unwrap_or_else(|_| glib::DateTime::from_unix_utc(0).unwrap());
    let year = now.year();
    let month = now.month() as u32;
    let day = now.day_of_month() as u32;
    day_index(year, month, day)
}

fn day_index(year: i32, month: u32, day: u32) -> i64 {
    days_from_civil_signed(year, month, day)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil_signed(year: i32, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let month = month as i64;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of `days_from_civil_signed`.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    ((y + if m <= 2 { 1 } else { 0 }) as i32, m as u32, d as u32)
}

fn month_name(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        _ => "Dec",
    }
}

/* ----------------------------- commit tree ------------------------------ */

/// Horizontal geometry of the graph column.
const LANE_W: f64 = 16.0;
const LANE_PAD: f64 = 6.0;

/// Width of the drawing area for `lanes` lanes.
fn graph_width(lanes: usize) -> i32 {
    (LANE_PAD * 2.0 + LANE_W * lanes.max(1) as f64) as i32
}

fn lane_x(lane: usize) -> f64 {
    LANE_PAD + lane as f64 * LANE_W + LANE_W / 2.0
}

/// Lane colours, cycled so neighbouring branches stay distinguishable.
fn lane_colour(index: usize) -> (f64, f64, f64) {
    const PALETTE: [(f64, f64, f64); 8] = [
        (0.322, 0.580, 0.886), // blue
        (0.247, 0.639, 0.302), // green
        (0.898, 0.718, 0.231), // gold
        (0.839, 0.271, 0.271), // red
        (0.557, 0.357, 0.769), // purple
        (0.086, 0.627, 0.522), // teal
        (0.831, 0.337, 0.561), // pink
        (0.878, 0.541, 0.235), // orange
    ];
    PALETTE[index % PALETTE.len()]
}

/// Vertical band `(row, y, height)` of every row, in list-box coordinates.
fn row_bands(list: &gtk::ListBox) -> Vec<(gtk::ListBoxRow, f64, f64)> {
    let mut bands = Vec::new();
    let mut child = list.first_child();
    while let Some(widget) = child {
        if let Ok(row) = widget.clone().downcast::<gtk::ListBoxRow>() {
            if let Some(bounds) = row.compute_bounds(list) {
                bands.push((
                    row,
                    f64::from(bounds.y()),
                    f64::from(bounds.height()),
                ));
            }
        }
        child = widget.next_sibling();
    }
    bands
}

/// Select the row under `y`, so clicking a dot behaves like clicking the row.
fn select_row_at(list: &gtk::ListBox, y: f64) {
    for (row, row_y, row_h) in row_bands(list) {
        if y >= row_y && y < row_y + row_h {
            if row.is_selectable() {
                list.select_row(Some(&row));
            }
            return;
        }
    }
}

/// Paint the lanes, merge curves and commit dots beside the history rows.
fn draw_commit_tree(cr: &gtk::cairo::Context, history: &gtk::ListBox, model: &git::GraphModel) {
    let bands = row_bands(history);
    if bands.is_empty() {
        return;
    }
    cr.set_line_width(2.0);
    cr.set_line_cap(gtk::cairo::LineCap::Round);
    cr.set_line_join(gtk::cairo::LineJoin::Round);

    for (index, layout) in model.rows.iter().enumerate() {
        let Some((_, row_y, row_h)) = bands.get(index) else {
            break;
        };
        let (y, height) = (*row_y, *row_h);
        let centre = y + height / 2.0;

        // Lanes running behind and through this row.
        for &lane in &layout.active {
            let (r, g, b) = lane_colour(lane);
            cr.set_source_rgba(r, g, b, 0.5);
            let x = lane_x(lane);
            cr.move_to(x, y);
            cr.line_to(x, y + height);
            let _ = cr.stroke();
        }
        // Lanes that stop here, running down to the dot or the merge point.
        for &lane in &layout.ending {
            let (r, g, b) = lane_colour(lane);
            cr.set_source_rgba(r, g, b, 0.5);
            let x = lane_x(lane);
            cr.move_to(x, y);
            cr.line_to(x, centre);
            let _ = cr.stroke();
        }
        // A lane opened by this row carries on downwards from its own dot.
        for &lane in &layout.continuing {
            if lane != layout.lane {
                continue;
            }
            let (r, g, b) = lane_colour(lane);
            cr.set_source_rgba(r, g, b, 0.5);
            let x = lane_x(lane);
            cr.move_to(x, centre);
            cr.line_to(x, y + height);
            let _ = cr.stroke();
        }
        // Curves: merges arriving at this dot, and branches leaving it.
        for link in &layout.links {
            let x1 = lane_x(link.from);
            let x2 = lane_x(link.to);
            let (r, g, b) = lane_colour(if link.into_dot { link.to } else { link.from });
            cr.set_source_rgba(r, g, b, 0.9);
            cr.move_to(x1, if link.into_dot { y } else { centre });
            if link.into_dot {
                cr.curve_to(x1, centre - 6.0, x2, y + 4.0, x2, centre);
            } else {
                let Some((_, target_y, target_h)) = bands.get(link.target_row) else {
                    continue;
                };
                let target = target_y + target_h / 2.0;
                let reach = ((target - centre).abs() * 0.45).clamp(6.0, 44.0);
                cr.curve_to(x1, centre + reach, x2, target - reach, x2, target);
            }
            let _ = cr.stroke();
        }

        // The commit itself.
        if layout.commit.is_some() {
            let x = lane_x(layout.lane);
            let (r, g, b) = lane_colour(layout.lane);
            cr.set_source_rgba(r, g, b, 0.3);
            cr.arc(x, centre, 6.5, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
            cr.set_source_rgb(r, g, b);
            cr.arc(x, centre, 4.0, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
        }
    }
}

fn run_action(
    root: &Path,
    message: &str,
    action: fn(&Path) -> Result<String, String>,
    status: gtk::Label,
    reload: Rc<dyn Fn(Option<String>)>,
) {
    status.set_text(message);
    status.set_visible(true);
    let root = root.to_path_buf();
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(action(&root));
    });
    glib::spawn_future_local(async move {
        match rx.recv().await {
            Ok(Ok(_)) => {
                status.set_text("Done");
                status.set_visible(true);
                reload(None);
            }
            Ok(Err(err)) => {
                status.set_text(&err);
                status.set_visible(true);
            }
            Err(_) => {}
        }
    });
}
