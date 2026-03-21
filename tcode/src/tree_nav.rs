use ratatui::widgets::ListState;

/// Trait for arena-based tree UI navigation.
///
/// Provides default implementations for move_up, move_down, toggle_collapse,
/// sync_list_state, and clamp_selection — shared across tree and permission UIs.
pub(crate) trait TreeNav {
    fn node_children(&self, idx: usize) -> &[usize];
    fn node_collapsed(&self, idx: usize) -> bool;
    fn set_node_collapsed(&mut self, idx: usize, collapsed: bool);
    fn visible(&self) -> &[usize];
    fn selected(&self) -> usize;
    fn set_selected(&mut self, idx: usize);
    /// Rebuild the visible list from the arena. Implementations should call
    /// `clamp_selection()` at the end.
    fn rebuild_visible(&mut self);

    fn move_down(&mut self) {
        let len = self.visible().len();
        if len > 0 && self.selected() < len - 1 {
            self.set_selected(self.selected() + 1);
        }
    }

    fn move_up(&mut self) {
        if self.selected() > 0 {
            self.set_selected(self.selected() - 1);
        }
    }

    fn toggle_collapse(&mut self) {
        let sel = self.selected();
        if let Some(&idx) = self.visible().get(sel)
            && !self.node_children(idx).is_empty()
        {
            let new_val = !self.node_collapsed(idx);
            self.set_node_collapsed(idx, new_val);
            self.rebuild_visible();
        }
    }

    fn sync_list_state(&self, list_state: &mut ListState) {
        if self.visible().is_empty() {
            list_state.select(None);
        } else {
            list_state.select(Some(self.selected()));
        }
    }

    /// Clamp selection to visible bounds.
    fn clamp_selection(&mut self) {
        let len = self.visible().len();
        if len > 0 && self.selected() >= len {
            self.set_selected(len - 1);
        }
    }
}
