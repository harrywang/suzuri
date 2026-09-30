use gpui::{Menu, MenuItem};

pub fn document_menu() -> Menu {
    Menu::new("Document").items([
        MenuItem::submenu(Menu::new("Format").items([
            MenuItem::action("Heading 1", markdown_writing::ToggleHeading1),
            MenuItem::action("Heading 2", markdown_writing::ToggleHeading2),
            MenuItem::action("Heading 3", markdown_writing::ToggleHeading3),
            MenuItem::action("Heading 4", markdown_writing::ToggleHeading4),
            MenuItem::action("Heading 5", markdown_writing::ToggleHeading5),
            MenuItem::action("Heading 6", markdown_writing::ToggleHeading6),
            MenuItem::action("Body", markdown_writing::ClearHeading),
            MenuItem::separator(),
            MenuItem::action("Bold", markdown_writing::ToggleBold),
            MenuItem::action("Italic", markdown_writing::ToggleItalic),
            MenuItem::action("Strikethrough", markdown_writing::ToggleStrikethrough),
            MenuItem::action("Inline Code", markdown_writing::ToggleInlineCode),
            MenuItem::separator(),
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
