#set page(paper: "a4", margin: 2.5cm)
#set text(size: 11pt)

= Feature tour

#import "lib.typ": greeting, version

#greeting — module version #version.

#let n = 3
#if n > 2 [n is bigger than two.] else [n is small.]

#for i in (1, 2, 3) [
  Item number #i.
]

#table(
  columns: (auto, 1fr, 60pt),
  [Name], [Description], [Qty],
  [Alpha], [First *thing* in the list], [10],
  [Beta], [Second thing, _italic_], [20],
)

#set text(font: "Times New Roman")
Body text after a font switch stays Times.

#for (k, v) in (a: 1, b: 2) [
  key #k = #v
]
