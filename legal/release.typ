// Shared layout for Sun Brewed Homestead legal releases.
//
// Every box a signer fills in emits `<field>` metadata: its Documenso
// field type, page and position (percent of the page). `build.sh` writes
// that metadata to `<name>.fields.json` beside the PDF, and the core sends
// exactly those fields to Documenso, so the boxes and the signing fields
// can't drift.
//
// Page: US Letter, 8.5 x 11 in. A release may run to several pages; the
// signature block is drawn at a fixed position at the foot of the last
// one only, and the body ends with a spacer as tall as that block, so
// text can't run under it (build.sh fails if the signature isn't on the
// last page). A release that runs past one page puts its last section in
// `signing[...]`, which keeps that text on the signature's page: a
// signature page with no terms on it could be attached to anything.

#let draft = sys.inputs.at("draft", default: "true") == "true"

#let sig = (
  signature: (x: 0.9in, y: 8.55in, w: 3.5in, h: 0.55in),
  date:      (x: 4.8in, y: 8.55in, w: 2.8in, h: 0.55in),
  name:      (x: 0.9in, y: 9.45in, w: 3.5in, h: 0.45in),
  email:     (x: 4.8in, y: 9.45in, w: 2.8in, h: 0.45in),
)

// Detail boxes above the signature block, filled in order by a release's
// `details` (documenso/releases.toml): typed in by the sender and locked,
// or left for the signer to fill in. One or two details take only the row
// nearest the signature, leaving the upper row's space to the text.
#let slots = (
  (x: 0.9in, y: 7.2in, w: 3.5in, h: 0.33in),
  (x: 4.8in, y: 7.2in, w: 2.8in, h: 0.33in),
  (x: 0.9in, y: 7.72in, w: 3.5in, h: 0.33in),
  (x: 4.8in, y: 7.72in, w: 2.8in, h: 0.33in),
)

#let amber = rgb("#C7771B")
#let soil = rgb("#3A2415")

#let pct(v, of) = calc.round(v / of * 100, digits: 3)

// A box to fill in, and the Documenso field that goes on it.
#let field-box(b, label, kind) = place(top + left, dx: b.x, dy: b.y, box(width: b.w, height: b.h + 0.22in)[
  #context [#metadata((
    kind: kind, page: here().page(), label: label,
    x: pct(b.x, 8.5in), y: pct(b.y, 11in), width: pct(b.w, 8.5in), height: pct(b.h, 11in),
  )) <field>]
  #rect(width: 100%, height: b.h, stroke: (bottom: 0.8pt + soil), fill: rgb("#FFF8EC"))
  #v(-0.08in)
  #text(size: 7.5pt, fill: soil.lighten(30%), tracking: 0.04em, upper(label))
])

#let minors-note = [Signed electronically. If the participant is under 18, a parent or legal guardian signs on their behalf.]

#let signature-block(details, note) = {
  assert(details.len() <= slots.len(), message: "at most four detail boxes")
  place(top + left, dx: 0.9in, dy: 8.3in, text(size: 7.5pt, fill: soil.lighten(30%), note))
  field-box(sig.signature, "Signature", "SIGNATURE")
  field-box(sig.date, "Date", "DATE")
  field-box(sig.name, "Printed name", "NAME")
  field-box(sig.email, "Email", "EMAIL")
  let first = if details.len() <= 2 { 2 } else { 0 }
  for (i, label) in details.enumerate() {
    field-box(slots.at(first + i), label, "TEXT")
  }
}

// A liability release, or any term that gives up a right for good (an
// irrevocable permission), set apart so no one can miss it: California
// courts refuse a release that is buried in a longer agreement without a
// heading or type that sets it apart (Leon v. Family Fitness Center,
// 1998). `warning` says in plain words what the box gives up. The signer
// initials each box separately, in a box that flows with the text; its
// `<field>` position is read from where the layout put it.
#let initials-size = (w: 1.05in, h: 0.36in)
#let initials(label) = box(width: initials-size.w, height: initials-size.h + 0.18in, {
  context {
    let p = here().position()
    [#metadata((
      kind: "INITIALS", page: p.page, label: label,
      x: pct(p.x, 8.5in), y: pct(p.y, 11in),
      width: pct(initials-size.w, 8.5in), height: pct(initials-size.h, 11in),
    )) <field>]
  }
  rect(width: 100%, height: initials-size.h, stroke: (bottom: 0.8pt + soil), fill: rgb("#FFF8EC"))
  v(-0.08in)
  text(size: 7pt, fill: soil.lighten(30%), tracking: 0.04em, upper(label))
})

// A short, friendly summary set above the terms. It says what the terms
// say, never more: it is labelled as a summary, and the terms control.
#let plain-words(body) = block(
  width: 100%, inset: (x: 0.14in, y: 0.1in), radius: 3pt, fill: rgb("#F3F6EC"),
  stroke: (left: 2.5pt + rgb("#6E8B3D")), breakable: false,
  {
    text(size: 10.5pt, weight: "bold")[In plain words]
    v(-0.2em)
    set text(size: 10pt)
    body
    v(-0.1em)
    text(size: 8.4pt, style: "italic")[This summary is here to help. The numbered sections below are the agreement itself.]
  },
)

#let release-box(heading, warning: [It gives up your right to sue us if you are hurt, including by our own carelessness.], body) = block(
  width: 100%, inset: (x: 0.12in, y: 0.09in), radius: 3pt,
  stroke: 1.2pt + amber, fill: rgb("#FFFBF4"), breakable: false,
  {
    grid(columns: (1fr, auto), column-gutter: 0.15in, align: (left + top, right + top),
      {
        text(size: 10.2pt, weight: "bold", tracking: 0.02em, upper(heading))
        linebreak()
        text(size: 8.4pt, style: "italic")[Please read this box carefully. #warning]
      },
      initials("Initials"),
    )
    v(0.04in)
    body
  },
)

#let watermark = if draft {
  rotate(-35deg, text(size: 64pt, fill: rgb("#E2963B").transparentize(82%), weight: "bold")[DRAFT · NOT APPROVED FOR USE])
}

#let letterhead = {
  grid(columns: (1fr, auto), align: (left + bottom, right + bottom),
    text(size: 8pt, tracking: 0.14em, fill: amber, weight: "bold")[SUN BREWED HOMESTEAD INC.],
    text(size: 7.5pt, fill: soil.lighten(35%))[11333 Moorpark St PMB 216, Studio City, CA 91602 · EIN 92-0510478]) // oss-publish:allow: the org's IRS-listed mailbox
  v(-0.2em)
  line(length: 100%, stroke: 0.6pt + amber)
}

#let setup(title, body) = {
  set document(title: title, author: "Sun Brewed Homestead Inc.")
  set text(font: "Libertinus Serif", size: 10pt, fill: soil)
  set par(justify: true, leading: 0.58em, spacing: 0.85em)
  body
}

// Where the body must end on the last page: above the minors note, or
// above the detail boxes (one row for up to two details, else two).
#let signature-top(details) = if details.len() == 0 { 8.25in } else if details.len() <= 2 { 7.55in } else { 7.05in }

#let bottom-margin = 0.8in

// Space for the signature block, kept clear below the text.
#let signature-space(details) = block(width: 100%, height: 11in - bottom-margin - signature-top(details), breakable: false)

#let release-details = state("release-details", ())

// The closing section, kept on the signature's page with the space for it.
#let signing(body) = {
  [#metadata(none) <signing>]
  block(width: 100%, breakable: false, { body; context signature-space(release-details.get()) })
}


// One document, signed at the foot of its last page.
#let release(title: "", intro: [], details: (), note: minors-note, body) = setup(title, {
  set page(paper: "us-letter", margin: (top: 0.8in, left: 0.9in, right: 0.9in, bottom: bottom-margin),
    // Drawn in the page foreground so coordinates are relative to the page,
    // not the text area.
    foreground: context if here().page() == counter(page).final().first() { signature-block(details, note) },
    background: watermark)
  release-details.update(details)
  letterhead
  text(size: 15pt, weight: "bold", title)
  v(0.1em)
  intro
  parbreak()
  body
  context if query(<signing>).len() == 0 { signature-space(details) }
  // Separate from the spacer, so a failure can't remove it and oscillate.
  context assert(query(<signing>).len() > 0 or counter(page).final().first() == 1,
    message: title + " runs past one page: put its last section in signing[...]")
})
