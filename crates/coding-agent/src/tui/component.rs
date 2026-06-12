//! Component-based UI architecture inspired by oh-my-pi.
//!
//! Each UI element is an independent `Component` that renders to `Vec<Line>`.
//! Components are composed via `Container`.

use ratatui::text::Line;

/// A UI component that renders itself into styled lines.
///
/// Components own their render cache and only re-render when content or width
/// changes. This mirrors oh-my-pi's `Component` interface where `render()`
/// returns a cached array reference when nothing changed.
pub trait Component {
    /// Render the component to lines at the given content width.
    fn render(&mut self, width: u16) -> Vec<Line<'static>>;

    /// Invalidate any cached rendering state.
    fn invalidate(&mut self) {}
}

/// A container that composes multiple components into a single render.
///
/// Renders children in order, concatenating their output lines.
/// Children are owned as `Box<dyn Component>` for polymorphic composition.
pub struct Container {
    children: Vec<Box<dyn Component>>,
}

impl Container {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    pub fn push(&mut self, child: Box<dyn Component>) {
        self.children.push(child);
    }

    pub fn len(&self) -> usize {
        self.children.len()
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }

    pub fn last_mut(&mut self) -> Option<&mut Box<dyn Component>> {
        self.children.last_mut()
    }
}

impl Component for Container {
    fn render(&mut self, width: u16) -> Vec<Line<'static>> {
        if self.children.is_empty() {
            return Vec::new();
        }

        let estimated: usize = self.children.len() * 8;
        let mut lines = Vec::with_capacity(estimated);
        for child in &mut self.children {
            lines.extend(child.render(width));
        }
        lines
    }

    fn invalidate(&mut self) {
        for child in &mut self.children {
            child.invalidate();
        }
    }
}
