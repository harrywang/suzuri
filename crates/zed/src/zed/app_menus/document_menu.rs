use gpui::{Menu, MenuItem};

pub fn document_menu() -> Menu {
    Menu::new("Document").items([
        MenuItem::action("Insert Citation…", citations::InsertCitation),
        MenuItem::action("Open Citation Source", citations::OpenSource),
        MenuItem::separator(),
        MenuItem::action(
            "Toggle Live Preview",
            markdown_live_preview::ToggleLivePreview,
        ),
        MenuItem::action("Preview Typst or LaTeX", typeset_preview::OpenLivePreview),
        MenuItem::separator(),
        export_submenu(),
    ])
}

// Also listed under File, where people look for Export first.
pub fn export_submenu() -> MenuItem {
    MenuItem::submenu(Menu::new("Export").items([
        MenuItem::action("PDF", markdown_export::ExportToPdf),
        MenuItem::action("Word", markdown_export::ExportToDocx),
        MenuItem::action("HTML", markdown_export::ExportToHtml),
        MenuItem::action("LaTeX", markdown_export::ExportToLatex),
        MenuItem::action("EPUB", markdown_export::ExportToEpub),
    ]))
}
