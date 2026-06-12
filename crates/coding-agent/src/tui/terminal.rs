use std::io::{self, Stdout};

use crossterm::event::{self, Event, KeyEvent, MouseEvent};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

pub type TuiBackend = CrosstermBackend<Stdout>;

/// Manages the terminal session for the TUI.
pub struct TerminalSession {
    pub terminal: Terminal<TuiBackend>,
}

impl TerminalSession {
    /// Enter raw mode and alternate screen with mouse capture enabled.
    pub fn enter() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            crossterm::event::EnableMouseCapture
        )?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    /// Restore the terminal to normal mode.
    pub fn restore(&mut self) -> anyhow::Result<()> {
        disable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            crossterm::event::DisableMouseCapture,
            LeaveAlternateScreen
        )?;
        self.terminal.show_cursor()?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}

/// Simplified input event for the app.
#[derive(Debug)]
pub enum InputEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Tick,
    None,
}

/// Poll for the next input event with a timeout.
pub fn poll_event(timeout_ms: u64) -> InputEvent {
    if event::poll(std::time::Duration::from_millis(timeout_ms)).unwrap_or(false) {
        match event::read() {
            Ok(Event::Key(key)) => InputEvent::Key(key),
            Ok(Event::Mouse(mouse)) => InputEvent::Mouse(mouse),
            Ok(Event::Resize(w, h)) => InputEvent::Resize(w, h),
            _ => InputEvent::None,
        }
    } else {
        InputEvent::Tick
    }
}
