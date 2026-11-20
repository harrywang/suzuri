// A small but representative document: text with ligatures, a link, shapes,
// gradients, and a second page. The live-preview test rasterizes page 0 of the
// recorded Tinymist stream for this file.
#set page(width: 200mm, height: 100mm, margin: 10mm)
#set text(font: "Libertinus Serif", size: 12pt)

= Typst live preview

Hello from *Tinymist*'s vector stream. #link("https://typst.app")[A link.]

#rect(width: 100%, height: 12mm, fill: gradient.linear(red, blue))

#circle(radius: 8mm, fill: rgb("#4f9cf9"))

#lorem(30)

#pagebreak()
= Second page

#table(
  columns: 2,
  [alpha], [beta],
  [gamma], [delta],
)
