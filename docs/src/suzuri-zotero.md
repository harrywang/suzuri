# Citations from Zotero

Use your Zotero collection to complete and check `[@key]` citations in Suzuri.
Better BibTeX keeps a `.bib` file updated in your local project; Suzuri reads that
file when it changes. No Zotero connection settings are needed in Suzuri.

## Set up an automatic export

1. Install [Zotero](https://www.zotero.org/download/) and
   [Better BibTeX for Zotero](https://retorque.re/zotero-better-bibtex/installation/).
   Better BibTeX is a Zotero plugin: install its `.xpi` through Zotero's
   **Tools → Plugins → gear menu → Install Plugin From File…**.
2. Put the references for your paper into a Zotero collection. In Better BibTeX's
   **Citation keys** preferences, keep the default citation-key formula,
   `auth.lower + shorttitle(3,3) + year`, unless your existing documents already use
   another convention. Use the key shown for the item rather than guessing it from
   its title. Keep keys stable once cited; see
   [Better BibTeX's citation-key guide](https://retorque.re/zotero-better-bibtex/citing/).
3. Right-click the collection in Zotero's left pane and choose
   **Export Collection…**. Select **Better BibTeX** and check **Keep updated**.
   **Better BibLaTeX** also works: Suzuri reads both formats and extracts the year
   from either `year` or a BibLaTeX `date` field. Choose the format your paper's
   typesetting workflow expects.
4. Save the export as `references.bib` inside the folder you open in Suzuri. For
   example, open `my-paper/`, containing `draft.md` and `references.bib`.
5. In Better BibTeX's **Automatic exports** preferences, verify that the export is
   registered and select **on change** for prompt updates. **On idle** waits until
   Zotero is idle; **paused** requires a manual export or resuming the schedule.
   Keep Zotero running while you expect it to export changes.

For the export controls, see
[Better BibTeX's automatic-export documentation](https://retorque.re/zotero-better-bibtex/exporting/auto/).

### Where to put the file

For a shared notes vault, put `references.bib` at the vault root. For a standalone
paper, export just its collection into that paper's project folder. A smaller
collection makes the export and completion list easier to manage.

Suzuri scans `.bib` files throughout the opened project's folders, including
subfolders; the file does not have to be named `references.bib`. Open the containing
folder as a project, rather than opening only a Markdown file. You do not need a
`bibliography:` frontmatter field for Suzuri's citation lookup. Such a field may
still be required by the tool you use to publish the document.

Keep citation keys unique across the bibliographies you open. Suzuri currently
shares its bibliography index across projects in the same application process.
Duplicate keys appear only once in completion, and conflicting metadata for the
same key is not a way to select a project-specific reference.

Treat the automatic export as generated data: edit the reference in Zotero.
Changes made directly to its `.bib` file can be overwritten by the next export.

## Write and check a citation

In a Markdown note, type `[@` followed by the start of a citation key. Select the
matching completion and add the closing `]` if it is not already present.
Completion inserts the key, not a formatted reference. Hover over the citation to
inspect its title, authors, year, and entry type when those fields are available.

Live preview displays citation chips. Once the bibliography index contains at
least one entry, keys that are absent from it receive error styling. With no
indexed entries, citations still render but are not flagged as unresolved, so the
absence of an error is not proof that your bibliography loaded.

These features resolve citation keys against metadata. They do not verify that a
paper supports your claim or generate a publication-style reference list. This
workflow also does not import Zotero PDFs or annotations.

### Try a small example without Zotero

Create a separate local folder, open it in Suzuri, and save the following two
files there. These are fictional references for checking the workflow; keep them
separate from your automatic export.

Save this bibliography as `references.bib`. The first entry uses BibTeX's `year`;
the second uses BibLaTeX's `date`:

```bibtex
@article{smith2020,
  title = {A Study of Things},
  author = {Smith, Jane},
  year = {2020},
}

@book{lee2024,
  title = {Research Notes},
  author = {Lee, Alex},
  date = {2024-03-01},
}
```

Save this note as `draft.md`. The last key is deliberately absent:

```markdown
# Citation check

A first reference [@smith2020].

Two references together [@smith2020; @lee2024].

This reference is missing [@missing2025].
```

Check that typing `[@smi` offers `smith2020`, hovering over `@lee2024` shows the
year 2024, and `@missing2025` receives unresolved-key styling in live preview.
To check reloading, change `smith2020`'s title in `references.bib` and save. Its
hover information should update without reopening `draft.md`.

For the Zotero workflow, repeat the title-change check on an item in your exported
collection. Wait for Better BibTeX to finish exporting, confirm the new title is
in the `.bib` file, then check it in Suzuri. This checks both halves of the chain.

## Troubleshooting

- **No Better BibTeX format or Keep updated option:** check that the plugin is
  installed in Zotero and choose **Better BibTeX** or **Better BibLaTeX**, rather
  than Zotero's standard BibTeX exporter.
- **Zotero changes do not reach the file:** check the export's destination and
  schedule under **Automatic exports**. Ensure the item belongs to the exported
  collection and wait for the export to finish. Suzuri cannot see a change until
  it reaches the `.bib` file on disk.
- **The file updates but the key is missing:** confirm that the `.bib` is inside
  the opened project and visible in the Project Panel. Check the exact exported
  key and that the note is recognized as Markdown. Use a local project for this
  guide; a local Zotero export does not transfer itself into an SSH project.
- **A previously working key is unresolved:** check whether the item was removed
  from the collection or its key changed. Update the citation or restore the key
  in Zotero, then let Better BibTeX export again.
- **Completion or highlighting flickers during export:** the current loader
  replaces a malformed or unreadable `.bib` file's indexed entries with an empty
  list; it does not retain the previous successful parse. If other bibliographies
  still have entries, affected keys can appear unresolved. If none remain,
  unresolved-key styling is disabled. A subsequent valid file update reloads the
  entries. Wait for the export to finish; if the problem persists, check the file
  for incomplete content and rerun the export in Better BibTeX.

Once this check works, use the same export setup for your real paper's collection.
For broader manual checks of Suzuri's writing and preview features, see
[suzuri-testbed](https://github.com/harrywang/suzuri-testbed).
