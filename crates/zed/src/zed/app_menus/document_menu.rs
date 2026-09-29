use gpui::{Menu, MenuItem};

pub fn document_menu() -> Menu {
    Menu::new("Document").items([
        MenuItem::submenu(Menu::new("Format").items([
            MenuItem::action("Bold", markdown_writing::ToggleBold),
            MenuItem::action("Italic", markdown_writing::ToggleItalic),
            MenuItem::action("Strikethrough", markdown_writing::ToggleStrikethrough),
            MenuItem::action("Inline Code", markdown_writing::ToggleInlineCode),
            MenuItem::action("Link", markdown_writing::InsertLink),
        ])),
        MenuItem::separator(),
        MenuItem::action("Insert Citation…", citations::InsertCitation),
        MenuItem::action("Open Citation Source", citations::OpenSource),
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
