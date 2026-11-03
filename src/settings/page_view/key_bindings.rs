//! The Key Bindings page as data: its master switch, then the list block the page owns.
//!
//! The binding list itself is NOT described. Its rows are the user's bindings: `bindings.rs`
//! rebuilds them from the config whenever the list changes, every row carries per-row actions
//! (toggle, delete, edit) rather than one `ControlField`, and the block's height is a canvas
//! consume the renderer replaces. It is drawn by the page in `extra_row`, on the page's own canvas,
//! so the card sequence stays the page's.

use super::*;

/// The page's own row key for the binding list.
pub(super) const BINDINGS_LIST_ROW: &str = "bindings_list";

/// The Key Bindings page's description.
pub(super) fn key_bindings_page_view(input: &PageInput) -> PageView {
    let cfg = &input.config;
    let mut w = RowWriter::new(
        SettingsPageId::KeyBindings,
        t("settings.sidebar_key_bindings"),
    );
    // The master switch is the page's only described row: it is the one control whose value lives
    // in the configuration and whose change the dispatcher handles by field.
    w.in_first_card("settings.header_key_bindings");
    w.push(
        switch(
            "settings.row_key_bindings_enabled",
            ControlField::KeyBindingsEnabled,
            cfg.key_bindings.enabled,
        )
        .desc("settings.desc_key_bindings_enabled")
        .into_spec(),
    );
    // The list opens its own card below the switch's, exactly where the inline builder put it.
    w.card("settings.header_key_binding_list");
    w.extra_row(BINDINGS_LIST_ROW, BINDINGS_BLOCK_H);
    w.finish()
}

/// The list block's provisional height: the renderer replaces it once the rows are built.
const BINDINGS_BLOCK_H: f64 = 200.0;

/// What the page draws itself: the binding list block and its empty-state hint, plus the
/// `SettingsUi` fields the list renderer and the master switch read (none of them are in the
/// shared slot table, because they are not one control each).
pub(crate) struct KeyBindingsHooks {
    /// The list block's canvas row, so the renderer can resize it.
    block_row: usize,
    /// The block's own view and its empty-state hint, owned by the page and handed to `SettingsUi`.
    document: *mut AnyObject,
    empty: *mut AnyObject,
}

impl Default for KeyBindingsHooks {
    fn default() -> Self {
        Self {
            block_row: 0,
            document: std::ptr::null_mut(),
            empty: std::ptr::null_mut(),
        }
    }
}

impl PageHooks for KeyBindingsHooks {
    unsafe fn extra_row(
        &mut self,
        canvas: &mut PageCanvas,
        rendered: &mut RenderedPage,
        _ui: &mut SettingsUi,
        spec: &PageRowSpec,
        origin_y: f64,
        _height: f64,
        _target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        let _ = rendered;
        if spec.key == BINDINGS_LIST_ROW {
            // The block belongs to the canvas: the renderer resizes it from the rows it holds, so
            // the page creates it and gives the canvas its `set_row_consume` handle.
            let (doc, empty) = super::page_builder::build_bindings_block(
                canvas.document(),
                canvas.content_width(),
                origin_y,
            );
            self.block_row = canvas.last_row();
            self.document = doc;
            self.empty = empty;
            canvas.set_row_consume(self.block_row, BINDINGS_BLOCK_H);
        }
        Ok(())
    }

    unsafe fn after_page(
        &mut self,
        _canvas: &mut PageCanvas,
        _rendered: &mut RenderedPage,
        ui: &mut SettingsUi,
        _target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        // The list renderer and the master switch's freeze read these; the described row binds the
        // switch itself through the shared table.
        ui.key_bindings_row = self.block_row;
        ui.key_bindings_doc = self.document;
        ui.key_bindings_empty = self.empty;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> PageInput {
        PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current())
    }

    /// The page is one described switch plus one page-drawn block: the switch is what the
    /// configuration stores, and the block is the user's binding list, which no description can
    /// carry (its rows and their per-row actions are rebuilt from the config).
    #[test]
    fn the_page_describes_its_master_switch_and_declares_the_list_block() {
        let view = key_bindings_page_view(&input());
        assert_eq!(view.page, SettingsPageId::KeyBindings);
        assert_eq!(view.rows.len(), 1, "only the master switch is described");
        assert_eq!(view.rows[0].label_key, "settings.row_key_bindings_enabled");
        assert_eq!(view.rows[0].field(), Some(ControlField::KeyBindingsEnabled));
        assert_eq!(
            view.rows[0].desc_key,
            Some("settings.desc_key_bindings_enabled")
        );
        assert_eq!(
            view.extra_rows.len(),
            1,
            "the list block is the page's own row"
        );
        assert_eq!(view.extra_rows[0].key, BINDINGS_LIST_ROW);
        assert_eq!(
            view.extra_rows[0].after_row, 0,
            "the block opens in its own card"
        );
        assert_eq!(view.extra_rows[0].card, 1, "after the switch's card");
        assert!(description_identity_conflict(&view).is_ok());
    }

    /// The switch's value is the config's, and the block's provisional height is the one the
    /// renderer replaces: a wrong height here moves every card below it.
    #[test]
    fn the_master_switch_reads_the_config_and_the_block_reserves_its_provisional_height() {
        let mut config = Config::default();
        config.key_bindings.enabled = !config.key_bindings.enabled;
        let view = key_bindings_page_view(&PageInput::from_config(
            config.clone(),
            None,
            crate::theme::ThemeInput::current(),
        ));
        let on = match view.rows[0].value {
            Value::Switch(on) => on,
            ref other => panic!("the master switch is a switch, not {other:?}"),
        };
        assert_eq!(on, config.key_bindings.enabled);
        assert_eq!(
            view.extra_rows[0].height,
            RowHeight::Fixed(BINDINGS_BLOCK_H),
            "the list block states its provisional height"
        );
        assert_eq!(BINDINGS_BLOCK_H, 200.0);
    }

    /// A field change must reach the described switch through the same update description the
    /// refill uses, or an open page keeps showing the old state after a reload.
    #[test]
    fn a_field_change_reaches_the_described_switch() {
        let mut config = Config::default();
        config.key_bindings.enabled = !config.key_bindings.enabled;
        let input =
            PageInput::from_config(config.clone(), None, crate::theme::ThemeInput::current());
        let updates = page_update_description(&input, &[SettingsPageId::KeyBindings]);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].field, ControlField::KeyBindingsEnabled);
        assert_eq!(
            updates[0].value,
            if config.key_bindings.enabled {
                "on"
            } else {
                "off"
            }
        );
    }
}
