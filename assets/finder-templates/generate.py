#!/usr/bin/env python3
"""Regenerate the blank Office templates bundled into "New File".

The app embeds these bytes verbatim (`include_bytes!` in src/finder_menu/new_file.rs), so a
regenerated template only takes effect after a rebuild. Run this from the repository root:

    python3 assets/finder-templates/generate.py

Requires python-docx, openpyxl and python-pptx. Two runs produce identical bytes. The libraries
stamp zip members and the document's own core properties with the current time, and openpyxl
overwrites `dcterms:modified` during save whatever the caller set, so every package is repacked with
a fixed timestamp and fixed core dates after it is written. The templates are committed, and a diff
that only shows "regenerated" noise would hide a real change to the document parts.

Two content fixes ride along: Word and PowerPoint files from these libraries already describe
themselves as Microsoft Office in docProps/app.xml, but openpyxl does not, and PowerPoint's template
states a 4:3 presentation format even after the slide size is changed. A "new file" that reports
"Openpyxl" in its properties is a defect the user can see.
"""

import datetime
import os
import re
import zipfile

from docx import Document
from docx.shared import Mm
from openpyxl import Workbook
from pptx import Presentation
from pptx.util import Inches

OUT = os.path.dirname(os.path.abspath(__file__))
FIXED = datetime.datetime(2000, 1, 1, 0, 0, 0)
FIXED_W3CDTF = "2000-01-01T00:00:00Z"
# A zip member timestamp is a DOS date, so it cannot predate 1980.
ZIP_TIME = (2000, 1, 1, 0, 0, 0)


def pin_core_dates(text):
    """Replace both core-property timestamps, which openpyxl rewrites on every save."""
    return re.sub(
        r"(<dcterms:(?:created|modified)[^>]*>)[^<]*(</dcterms:(?:created|modified)>)",
        lambda match: match.group(1) + FIXED_W3CDTF + match.group(2),
        text,
    )


def replace_once(before, after):
    def patch(text):
        if before not in text:
            raise SystemExit(f"{before!r} not in the package part")
        return text.replace(before, after)

    return patch


def drop(pattern):
    """Remove the single occurrence of `pattern`, which is what makes the removal meaningful."""
    return replace_pattern(pattern, "")


def replace_pattern(pattern, replacement):
    def patch(text):
        patched, count = re.subn(pattern, replacement, text)
        if count != 1:
            raise SystemExit(f"{pattern!r} matched {count} times, expected once")
        return patched

    return patch


def repack(path, patches):
    """Rewrite every zip member with a fixed timestamp, applying an optional text patch per part.

    Every other member is copied byte-for-byte and in its original order, because Office reads some
    packages in the order the parts were written.
    """
    with zipfile.ZipFile(path) as source:
        entries = [(info.filename, source.read(info.filename)) for info in source.infolist()]
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as target:
        for name, data in entries:
            if name in patches:
                data = patches[name](data.decode("utf-8")).encode("utf-8")
            info = zipfile.ZipInfo(name, date_time=ZIP_TIME)
            info.compress_type = zipfile.ZIP_DEFLATED
            target.writestr(info, data)


def blank_docx(path):
    document = Document()
    section = document.sections[0]
    section.page_width, section.page_height = Mm(210), Mm(297)
    section.top_margin = section.bottom_margin = Mm(25.4)
    section.left_margin = section.right_margin = Mm(25.4)
    properties = document.core_properties
    properties.author = properties.last_modified_by = ""
    properties.title = properties.subject = properties.comments = ""
    properties.created = properties.modified = FIXED
    properties.revision = 1
    document.save(path)
    repack(
        path,
        {
            "docProps/core.xml": pin_core_dates,
            # The template's author line, from python-docx's own default document, names en-US as
            # the default editing language (and ja-JP as the East Asian theme language). Shipping
            # that fixes every new document to English proofing -- Chinese text typed into it is
            # flagged as misspelled -- and Word compares it against the primary language from
            # System Settings. With the declarations dropped, Word applies the user's own default.
            "word/styles.xml": drop(r'\s*<w:lang w:val="en-US" w:eastAsia="en-US" w:bidi="ar-SA"\s*/>'),
            # Word 2010 kept a second style part for effects; it carries its own copy of the
            # default language, and Word falls back to it when the main styles part is missing.
            "word/stylesWithEffects.xml": drop(
                r'\s*<w:lang w:val="en-US" w:eastAsia="en-US" w:bidi="ar-SA"\s*/>'
            ),
            "word/settings.xml": drop(r"\s*<w:themeFontLang[^>]*/>"),
        },
    )


def blank_xlsx(path):
    workbook = Workbook()
    workbook.active.title = "Sheet1"
    properties = workbook.properties
    properties.creator = properties.lastModifiedBy = ""
    properties.title = properties.subject = properties.description = ""
    properties.created = properties.modified = FIXED
    workbook.save(path)
    repack(
        path,
        {
            "docProps/core.xml": pin_core_dates,
            "docProps/app.xml": replace_once(
                "<Application>Microsoft Excel Compatible / Openpyxl 3.1.5</Application>"
                "<AppVersion>3.1</AppVersion>",
                "<Application>Microsoft Excel</Application><AppVersion>16.0000</AppVersion>",
            ),
        },
    )


def blank_pptx(path):
    presentation = Presentation()
    # python-pptx's default template is a 4:3 deck. Changing only the slide size leaves every
    # placeholder laid out on the old canvas, so a new "Title and Content" slide would sit against
    # the left edge with a band of empty space on the right. Widen the master and every layout the
    # way PowerPoint's own "Maximize" option does when a deck is switched to widescreen.
    ratio = Inches(13.333) / presentation.slide_width
    presentation.slide_width, presentation.slide_height = Inches(13.333), Inches(7.5)
    # Snapshot the geometry before writing: a layout placeholder that inherits its position reads
    # the master's, so scaling the master first and then the layout would apply the ratio twice.
    targets = []
    for container in (presentation.slide_masters[0], *presentation.slide_layouts):
        for shape in container.shapes:
            geometry = (shape.left, shape.top, shape.width, shape.height)
            if None in geometry:
                continue
            targets.append((shape, geometry))
    for shape, (left, top, width, height) in targets:
        # All four values are written, not just the horizontal pair. A layout placeholder that
        # inherits its box reads the inherited values, and writing one axis alone creates a local
        # xfrm whose other axis stays at zero (measured: title boxes at y=0, height=0).
        shape.left = int(round(left * ratio))
        shape.top = top
        shape.width = int(round(width * ratio))
        shape.height = height
    properties = presentation.core_properties
    properties.author = properties.last_modified_by = ""
    properties.title = properties.subject = properties.comments = ""
    properties.created = properties.modified = FIXED
    properties.revision = 1
    presentation.save(path)
    repack(
        path,
        {
            "docProps/core.xml": pin_core_dates,
            "docProps/app.xml": replace_once(
                "<PresentationFormat>On-screen Show (4:3)</PresentationFormat>",
                "<PresentationFormat>Widescreen</PresentationFormat>",
            ),
            # The canvas size is in the attributes; the type still names the template's old deck.
            "ppt/presentation.xml": replace_pattern(
                r'(<p:sldSz[^>]*type=")screen4x3(")',
                r"\1screen16x9\2",
            ),
        },
    )


def drop_all(pattern):
    """Remove every occurrence of `pattern`, requiring at least one."""

    def patch(text):
        patched, count = re.subn(pattern, "", text)
        if count == 0:
            raise SystemExit(f"{pattern!r} matched nothing")
        return patched

    return patch


def strip_pptx_languages(path):
    """Remove the hard-coded proofing language from the deck's text.

    DrawingML repeats lang="en-US" on every run property: the presentation's default text style,
    the master and all eleven layouts. Left in place, PowerPoint proofreads text as American
    English whatever the user's own language is, which is the same defect the Word template had.
    """
    with zipfile.ZipFile(path) as package:
        names = [
            name
            for name in package.namelist()
            if name.startswith("ppt/")
            and name.endswith(".xml")
            and 'lang="en-US"' in package.read(name).decode("utf-8")
        ]
    if not names:
        raise SystemExit("blank.pptx names no language to strip")
    repack(path, {name: drop_all(r'\s+lang="en-US"') for name in names})


def verify_docx(path):
    """Assert the document carries no forced editing language and still opens as a package."""
    with zipfile.ZipFile(path) as package:
        styles = package.read("word/styles.xml").decode("utf-8")
        effects = package.read("word/stylesWithEffects.xml").decode("utf-8")
        settings = package.read("word/settings.xml").decode("utf-8")
    for name, part in (("styles", styles), ("stylesWithEffects", effects)):
        if "<w:lang" in part:
            raise SystemExit(f"blank.docx still pins a default editing language in {name}")
    if "<w:themeFontLang" in settings:
        raise SystemExit("blank.docx still pins a theme language")
    Document(path)


def verify_pptx(path):
    """Assert the shipped deck is actually widescreen, not a 4:3 layout on a wide canvas.

    A placeholder that merely fits inside the canvas proves nothing: the unscaled 4:3 layout fits
    too. What a user sees is the leftover band, so the check is that the wide placeholders are
    centred, i.e. the left and right gaps match.
    """
    presentation = Presentation(path)
    width = presentation.slide_width
    for layout in presentation.slide_layouts:
        for shape in layout.shapes:
            left, top, shape_width, height = shape.left, shape.top, shape.width, shape.height
            if None in (left, top, shape_width, height):
                continue
            if shape_width <= 0 or height <= 0:
                raise SystemExit(
                    f"layout {layout.name!r} placeholder {shape.name!r} has an empty box: "
                    f"{shape_width} x {height}"
                )
            if top + height > presentation.slide_height:
                raise SystemExit(
                    f"layout {layout.name!r} placeholder {shape.name!r} runs past the canvas bottom"
                )
            if shape_width < width * 0.8:
                continue
            left_gap = left
            right_gap = width - (left + shape_width)
            if abs(left_gap - right_gap) > width * 0.02:
                raise SystemExit(
                    f"layout {layout.name!r} placeholder {shape.name!r} is not centred: "
                    f"left {left_gap} vs right {right_gap} on {width}"
                )
    with zipfile.ZipFile(path) as package:
        parts = {
            name: package.read(name).decode("utf-8")
            for name in package.namelist()
            if name.startswith("ppt/") and name.endswith(".xml")
        }
    if 'type="screen16x9"' not in parts["ppt/presentation.xml"]:
        raise SystemExit("blank.pptx does not declare a widescreen canvas")
    for name, part in parts.items():
        if 'lang="en-US"' in part:
            raise SystemExit(f"blank.pptx still pins a proofing language in {name}")


def main():
    blank_docx(os.path.join(OUT, "blank.docx"))
    verify_docx(os.path.join(OUT, "blank.docx"))
    blank_xlsx(os.path.join(OUT, "blank.xlsx"))
    blank_pptx(os.path.join(OUT, "blank.pptx"))
    strip_pptx_languages(os.path.join(OUT, "blank.pptx"))
    verify_pptx(os.path.join(OUT, "blank.pptx"))
    for name in ("blank.docx", "blank.xlsx", "blank.pptx"):
        print(name, os.path.getsize(os.path.join(OUT, name)), "bytes")


if __name__ == "__main__":
    main()
