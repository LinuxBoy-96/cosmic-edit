// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    iced::{advanced::graphics::text::font_system, Point},
    widget,
};
use cosmic_files::mime_icon::{mime_for_path, mime_icon, FALLBACK_MIME_ICON};
use cosmic_text::{
    Attrs, AttrsList, Buffer, Cursor, Edit, Selection, Shaping, SyntaxEditor, ViEditor, Wrap,
};
use regex::Regex;
use std::{
    fs,
    io,
    path::{self, PathBuf},
    sync::{Arc, Mutex},
};

use crate::{config::Config, git::GitDiff, SYNTAX_SYSTEM};

pub enum Tab {
    Editor(EditorTab),
    GitDiff(GitDiffTab),
}

impl Tab {
    pub fn title(&self) -> String {
        match self {
            Self::Editor(tab) => tab.title(),
            Self::GitDiff(tab) => tab.title.clone(),
        }
    }
}

pub struct GitDiffTab {
    pub title: String,
    pub diff: GitDiff,
}

pub struct EditorTab {
    pub path_opt: Option<PathBuf>,
    attrs: Attrs<'static>,
    pub editor: Mutex<ViEditor<'static, 'static>>,
    pub context_menu: Option<Point>,
    zoom_adj: i8,
}

impl EditorTab {
    pub fn new(config: &Config) -> Self {
        let attrs = crate::monospace_attrs();

        let mut buffer = Buffer::new_empty(config.metrics(0));

        // Ici, dans TON build, set_wrap / set_tab_width / set_text demandent FontSystem.
        {
            let mut fs = font_system().write().expect("font system write");
            let fs = fs.raw();

            buffer.set_wrap(
                fs,
                if config.word_wrap {
                    Wrap::WordOrGlyph
                } else {
                    Wrap::None
                },
            );
            buffer.set_tab_width(fs, config.tab_width);
            buffer.set_text(fs, "", &attrs, Shaping::Advanced, None);
        }

        let syntax_editor = SyntaxEditor::new(
            Arc::new(buffer),
            SYNTAX_SYSTEM.get().expect("SYNTAX_SYSTEM not initialized"),
            config.syntax_theme(),
        )
        .expect("SyntaxEditor::new failed");

        let mut tab = Self {
            path_opt: None,
            attrs,
            editor: Mutex::new(ViEditor::new(syntax_editor)),
            context_menu: None,
            zoom_adj: 0,
        };

        tab.set_config(config);
        tab
    }

    pub fn title(&self) -> String {
        self.path_opt
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "Untitled".to_string())
    }

    pub fn icon(&self, size: u16) -> cosmic::widget::Icon {
        match self.path_opt.as_ref() {
            Some(path) => {
                let mime = mime_for_path(path, None, false);
                widget::icon(mime_icon(mime, size)).size(size)
            }
            None => crate::icon_cache_get(FALLBACK_MIME_ICON, size),
        }
    }

    pub fn zoom_adj(&self) -> i8 {
        self.zoom_adj
    }

    pub fn set_zoom_adj(&mut self, zoom_adj: i8) {
        self.zoom_adj = zoom_adj;
    }

    pub fn set_config(&mut self, config: &Config) {
        let mut editor = self.editor.lock().expect("editor lock");

        editor.set_auto_indent(config.auto_indent);
        editor.set_passthrough(!config.vim_bindings);

        let mut fs = font_system().write().expect("font system write");
        let fs_raw = fs.raw();
        let mut ed = editor.borrow_with(fs_raw);

        ed.update_theme(config.syntax_theme());

        ed.with_buffer_mut(|buffer| {
            buffer.set_metrics(config.metrics(self.zoom_adj));

            // IMPORTANT: ici, ton build veut les versions SANS FontSystem
            buffer.set_wrap(if config.word_wrap {
                Wrap::WordOrGlyph
            } else {
                Wrap::None
            });
            buffer.set_tab_width(config.tab_width);
        });
    }

    pub fn open(&mut self, path: PathBuf) {
        let absolute = match fs::canonicalize(&path) {
            Ok(ok) => ok,
            Err(_) => path::absolute(&path).unwrap_or(path),
        };

        let mut editor = self.editor.lock().expect("editor lock");
        let mut fs = font_system().write().expect("font system write");
        let mut ed = editor.borrow_with(fs.raw());

        match ed.load_text(&absolute, self.attrs.clone()) {
            Ok(()) => {
                self.path_opt = Some(absolute);
                ed.set_changed(false);
            }
            Err(err) => {
                if err.kind() == io::ErrorKind::NotFound {
                    self.path_opt = Some(absolute);
                    ed.with_buffer_mut(|buffer| {
                        // IMPORTANT: ici aussi, ton build veut set_text SANS FontSystem
                        buffer.set_text("", &self.attrs, Shaping::Advanced, None);
                    });
                    ed.set_changed(true);
                } else {
                    log::error!("failed to open {:?}: {}", absolute, err);
                }
            }
        }
    }

    pub fn reload(&mut self) {
        let Some(path) = self.path_opt.clone() else { return };

        let mut editor = self.editor.lock().expect("editor lock");
        let mut fs = font_system().write().expect("font system write");
        let mut ed = editor.borrow_with(fs.raw());

        match ed.load_text(&path, self.attrs.clone()) {
            Ok(()) => ed.set_changed(false),
            Err(err) => log::error!("failed to reload {:?}: {}", path, err),
        }
    }

    pub fn changed(&self) -> bool {
        self.editor.lock().expect("editor lock").changed()
    }

    fn get_text_locked(editor: &mut ViEditor<'static, 'static>) -> String {
        let mut fs = font_system().write().expect("font system write");
        let ed = editor.borrow_with(fs.raw());

        ed.with_buffer(|buffer| {
            let mut out = String::new();
            for (i, line) in buffer.lines.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                out.push_str(line.text());
            }
            out
        })
    }

    pub fn save(&mut self) {
        let Some(path) = self.path_opt.clone() else { return };

        let mut editor = self.editor.lock().expect("editor lock");
        let text = Self::get_text_locked(&mut editor);

        match fs::write(&path, text) {
            Ok(()) => {
                let mut fs = font_system().write().expect("font system write");
                let mut ed = editor.borrow_with(fs.raw());
                ed.set_changed(false);
            }
            Err(err) => log::error!("failed to save {:?}: {}", path, err),
        }
    }

    // --- Find / replace (used by main.rs) ---

    pub fn search(&self, regex: &Regex, forwards: bool, wrap_around: bool) -> bool {
        let mut editor = self.editor.lock().expect("editor lock");
        let text = Self::get_text_locked(&mut editor);

        let mut cursor = editor.cursor();
        let mut wrapped = false;

        loop {
            let (start, end) = if forwards {
                (cursor.index, text.len())
            } else {
                (0, cursor.index)
            };

            let hay = &text[start..end];

            let found = if forwards {
                regex.find(hay).map(|m| (start + m.start(), m.end() - m.start()))
            } else {
                regex
                    .find_iter(hay)
                    .last()
                    .map(|m| (m.start(), m.end() - m.start()))
            };

            if let Some((idx, len)) = found {
                let mut anchor = Cursor::default();
                anchor.index = idx;

                let mut end_cur = Cursor::default();
                end_cur.index = idx + len;

                editor.set_cursor(end_cur);
                editor.set_selection(Selection::Normal(anchor));
                return true;
            }

            if wrap_around && !wrapped {
                cursor.index = if forwards { 0 } else { text.len() };
                wrapped = true;
                continue;
            }

            return false;
        }
    }

    pub fn replace(&self, regex: &Regex, replacement: &str, forwards: bool) -> bool {
        let mut editor = self.editor.lock().expect("editor lock");
        let text = Self::get_text_locked(&mut editor);

        let cursor = editor.cursor();
        let (start, end) = if forwards {
            (cursor.index, text.len())
        } else {
            (0, cursor.index)
        };

        let hay = &text[start..end];

        let found = if forwards {
            regex.find(hay).map(|m| (start + m.start(), m.end() - m.start()))
        } else {
            regex
                .find_iter(hay)
                .last()
                .map(|m| (m.start(), m.end() - m.start()))
        };

        let Some((idx, len)) = found else { return false };

        let mut start_cur = Cursor::default();
        start_cur.index = idx;

        let mut end_cur = Cursor::default();
        end_cur.index = idx + len;

        let mut fs = font_system().write().expect("font system write");
        let mut ed = editor.borrow_with(fs.raw());

        ed.delete_range(start_cur, end_cur);
        let new_cur = ed.insert_at(start_cur, replacement, None::<AttrsList>);

        editor.set_cursor(new_cur);
        editor.set_selection(Selection::Normal(new_cur));
        true
    }
}
