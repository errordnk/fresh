//! Fresh as an editor inside another program's screen (farsh's F3, F4 and
//! quick view): the host owns the terminal, the input and the event loop, and
//! drives one editor per file through a [`Session`] — it ticks it, hands it
//! input, gives it a `ratatui::Frame` of the size it should fill, and asks
//! whether it wants another frame soon (an animation).
//!
//! A session is one file and the editor alone: no plugins, no LSP, no update
//! checks, no workspace or session restore (a file launch), and none of the
//! chrome a host draws itself — the status bar and the `~` lines past the end
//! are off on top of the fork's defaults (menu, tabs, prompt line, horizontal
//! scrollbar already off). The user's own config (`~/.config/fresh`) is read
//! and kept otherwise: theme, keymap and editor options are the host's to
//! override through [`EmbedOptions`] and [`Session::config_mut`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;

use crate::app::Editor;
use crate::config::Config;
use crate::config_io::DirectoryContext;

/// How a session is opened.
#[derive(Debug, Clone, Default)]
pub struct EmbedOptions {
    /// The file opens read-only (a viewer): edits are refused, the cursor
    /// still moves, selects and searches.
    pub read_only: bool,
    /// The folder the session counts as its project (relative paths, the
    /// config's project layer); the file's folder when `None`.
    pub working_dir: Option<PathBuf>,
    /// Applied to the config after the embedding defaults: the host's own
    /// settings (tab size, keymap, theme...).
    pub configure: Option<fn(&mut Config)>,
    /// A theme as JSON (a partial one may `extends` a builtin), applied to the
    /// session without touching any file: the host's colors.
    pub theme_json: Option<String>,
    /// Where the config, data and caches are; the user's (`~/.config/fresh`,
    /// `~/.local/share/fresh`...) when `None`. Tests pass a temporary one.
    pub dirs: Option<DirectoryContext>,
}

/// One file in an editor that draws into the host's frames.
pub struct Session {
    editor: Editor,
    path: PathBuf,
}

/// The config a session starts from: the user's, then the embedding defaults.
pub fn embedded_config(dir_context: &DirectoryContext, working_dir: &Path) -> Config {
    let mut config = Config::load_with_layers(dir_context, working_dir);
    apply_embedding_defaults(&mut config);
    config
}

/// What a host draws or does itself, or what a one-file editor inside it has
/// no use for. Animations stay (the user's call): the host redraws while
/// [`Session::next_frame`] asks for it.
pub fn apply_embedding_defaults(config: &mut Config) {
    config.editor.show_status_bar = false;
    config.editor.show_tilde = false;
    config.editor.show_menu_bar = false;
    config.editor.show_tab_bar = false;
    config.editor.show_prompt_line = false;
    config.lsp_enabled = false;
    config.check_for_updates = false;
}

impl Session {
    /// Opens `path` in an editor of `cols` x `rows` cells.
    pub fn open(path: &Path, cols: u16, rows: u16, options: EmbedOptions) -> Result<Self> {
        let path = std::path::absolute(path)?;
        let working_dir = options
            .working_dir
            .clone()
            .or_else(|| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let dir_context = match options.dirs.clone() {
            Some(dirs) => dirs,
            None => DirectoryContext::from_system()?,
        };
        let mut config = embedded_config(&dir_context, &working_dir);
        if let Some(configure) = options.configure {
            configure(&mut config);
        }
        crate::i18n::init_with_config(config.locale.as_option());
        let connection = Arc::new(crate::services::authority::Connection::plain(
            crate::services::authority::Authority::local(
                Arc::new(crate::services::workspace_trust::WorkspaceTrust::permissive()),
                Arc::new(crate::services::env_provider::EnvProvider::inactive()),
            ),
        ));
        let mut editor = Editor::with_working_dir_opts(
            config,
            cols,
            rows,
            Some(working_dir),
            dir_context,
            false, // no plugins: the host has its own (farsh: Lua, later)
            crate::view::color_support::ColorCapability::TrueColor,
            connection,
            false, // nothing to defer without plugins
            false, // not the orchestrator
            true,  // a file launch: this file, no session
        )?;
        editor.set_workspace_persistence(false);
        if let Some(json) = &options.theme_json {
            let theme = crate::view::theme::Theme::from_json(json).map_err(|e| anyhow::anyhow!(e))?;
            *editor.theme.write().unwrap() = theme;
        }
        editor.open_file(&path)?;
        let mut session = Self { editor, path };
        if options.read_only {
            session.set_read_only(true);
        }
        Ok(session)
    }

    /// The file the session shows.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Opens another file in place of this one (the quick view following the
    /// cursor of a panel); the read-only state carries over.
    ///
    /// The file before is closed, its unsaved changes dropped (a viewer has
    /// none): a session is one file, not a tab per file looked at, and a buffer
    /// out of sight would keep its timers armed and the host redrawing.
    pub fn open_other(&mut self, path: &Path) -> Result<()> {
        let read_only = self.read_only();
        let path = std::path::absolute(path)?;
        let before = self.editor.active_buffer();
        let now = self.editor.open_file(&path)?;
        if now != before {
            self.editor.force_close_buffer(before)?;
        }
        self.path = path;
        self.set_read_only(read_only);
        Ok(())
    }

    /// How many buffers the session holds (one, the file shown).
    pub fn buffer_count(&self) -> usize {
        self.editor.active_window().buffers.len()
    }

    /// Read-only (a viewer) or editable (F3 → F4 in the same session).
    pub fn set_read_only(&mut self, read_only: bool) {
        self.editor.active_state_mut().editing_disabled = read_only;
    }

    pub fn read_only(&self) -> bool {
        self.editor.active_window().is_editing_disabled()
    }

    /// The editor's own housekeeping (async results, timers, file changes on
    /// disk); `true` when the next frame differs.
    pub fn tick(&mut self) -> Result<bool> {
        crate::app::editor_tick(&mut self.editor, || Ok(()))
    }

    /// Draws the editor into `frame` (its whole area).
    pub fn render(&mut self, frame: &mut ratatui::Frame) {
        self.editor.render(frame);
    }

    /// A key, mouse or paste event in the session's coordinates; `true` when
    /// the next frame differs.
    pub fn handle(&mut self, event: fresh_input_parser::Event) -> Result<bool> {
        self.editor.handle_input_event(event)
    }

    /// The area the session draws into changed.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.editor.resize(cols, rows);
    }

    /// When the session wants a frame without any input (an animation, a
    /// spinner, a wheel scroll still walking): the host redraws by then.
    pub fn next_frame(&mut self) -> Option<Instant> {
        let animating = self.editor.active_window().animations.is_active()
            || self
                .editor
                .active_window_mut()
                .animations
                .take_settle_frame()
            || self.editor.has_pending_wheel_scroll()
            || self.editor.has_drag_autoscroll();
        if animating {
            return Some(Instant::now());
        }
        self.editor.next_periodic_redraw_deadline()
    }

    /// The user asked to leave (the keymap's quit): the host closes the session.
    pub fn should_quit(&self) -> bool {
        self.editor.should_quit()
    }

    /// Whether the buffer has changes not saved.
    pub fn modified(&self) -> bool {
        self.editor.active_state().buffer.is_modified()
    }

    /// The editor, for what this API does not cover yet.
    pub fn editor(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// Ends the session: the quit-time persistence that applies to a file
    /// launch (prompt histories, recovery cleanup), no workspace.
    pub fn close(mut self) -> Result<()> {
        if let Err(error) = self.editor.persist_on_exit(false) {
            tracing::debug!("embedded session: quit-time persistence incomplete: {error}");
        }
        self.editor.end_recovery_session()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> fresh_input_parser::Event {
        fresh_input_parser::Event::from(crossterm::event::Event::Key(KeyEvent::new(
            code, modifiers,
        )))
    }

    fn screen(session: &mut Session, terminal: &mut Terminal<TestBackend>) -> Vec<String> {
        let _ = session.tick();
        terminal.draw(|frame| session.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    fn options(dirs: &Path, read_only: bool) -> EmbedOptions {
        EmbedOptions {
            read_only,
            dirs: Some(DirectoryContext::for_testing(dirs)),
            ..EmbedOptions::default()
        }
    }

    #[test]
    fn a_session_shows_the_file_alone_edits_and_saves_it() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("note.txt");
        std::fs::write(&file, "hello embedded\nsecond line\n").unwrap();
        let mut session =
            Session::open(&file, 40, 8, options(&temp.path().join("dirs"), false)).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        let rows = screen(&mut session, &mut terminal);
        // the text is there; no status bar, no tab bar, no menu, no `~` past the end
        assert!(
            rows.iter().any(|r| r.contains("hello embedded")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().all(|r| !r.contains("note.txt")),
            "no chrome naming the file: {rows:#?}"
        );
        assert!(
            rows.iter().all(|r| !r.trim_start().starts_with('~')),
            "{rows:#?}"
        );
        assert!(!session.read_only() && !session.modified());
        // type at the start of the first line, save with the keymap's Ctrl+S
        session
            .handle(key(KeyCode::Char('X'), KeyModifiers::NONE))
            .unwrap();
        assert!(session.modified());
        session
            .handle(key(KeyCode::Char('s'), KeyModifiers::CONTROL))
            .unwrap();
        for _ in 0..200 {
            let _ = session.tick();
            if std::fs::read_to_string(&file)
                .unwrap()
                .starts_with("Xhello")
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(std::fs::read_to_string(&file)
            .unwrap()
            .starts_with("Xhello embedded"));
        session.close().unwrap();
    }

    #[test]
    fn a_read_only_session_refuses_edits_until_it_is_made_editable() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("view.txt");
        std::fs::write(&file, "look only\n").unwrap();
        let mut session =
            Session::open(&file, 40, 8, options(&temp.path().join("dirs"), true)).unwrap();
        assert!(session.read_only());
        session
            .handle(key(KeyCode::Char('Z'), KeyModifiers::NONE))
            .unwrap();
        assert!(!session.modified(), "a viewer does not change the text");
        // F3 -> F4: the same session becomes an editor
        session.set_read_only(false);
        session
            .handle(key(KeyCode::Char('Z'), KeyModifiers::NONE))
            .unwrap();
        assert!(session.modified());
        // another file in place of this one keeps the state it had
        let other = temp.path().join("other.txt");
        std::fs::write(&other, "the next file\n").unwrap();
        session.set_read_only(true);
        session.open_other(&other).unwrap();
        assert!(session.read_only());
        assert_eq!(session.path(), std::path::absolute(&other).unwrap());
        assert_eq!(
            session.buffer_count(),
            1,
            "the file before is closed, not kept as a tab"
        );
        // and again over many files: still one, and nothing left asking for frames
        for round in 0..5 {
            let next = temp.path().join(format!("more{round}.txt"));
            std::fs::write(&next, format!("file {round}\n")).unwrap();
            session.open_other(&next).unwrap();
        }
        assert_eq!(session.buffer_count(), 1);
        // the same file again is not closed under itself
        session.open_other(&other).unwrap();
        session.open_other(&other).unwrap();
        assert_eq!(session.buffer_count(), 1);
        assert_eq!(session.path(), std::path::absolute(&other).unwrap());
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        let rows = screen(&mut session, &mut terminal);
        assert!(
            rows.iter().any(|r| r.contains("the next file")),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_host_theme_colors_the_text_area() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("note.txt");
        std::fs::write(&file, "hello
").unwrap();
        let mut opts = options(&temp.path().join("dirs"), true);
        opts.theme_json = Some(
            r#"{"name":"host","extends":"builtin://dark","editor":{"bg":[0,0,128],"after_eof_bg":[0,0,128],"fg":[0,255,255]}}"#.into(),
        );
        let mut session = Session::open(&file, 20, 4, opts).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
        let _ = screen(&mut session, &mut terminal);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(15, 3)].bg, ratatui::style::Color::Rgb(0, 0, 128));
        let first = (0..20).find(|x| buffer[(*x, 0)].symbol() == "h").unwrap();
        assert_eq!(buffer[(first, 0)].fg, ratatui::style::Color::Rgb(0, 255, 255));
        assert_eq!(buffer[(10, 3)].bg, ratatui::style::Color::Rgb(0, 0, 128));
    }
}
