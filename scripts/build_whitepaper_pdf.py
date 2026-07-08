#!/usr/bin/env python3
"""Render docs/whitepaper-zh.md into a polished, reader-friendly PDF.

Layout: A4, CJK-aware fonts (STHeiti), cover page, footer page numbers,
striped tables, shaded code blocks. Parses the limited Markdown subset the
whitepaper actually uses (headings, tables, fenced code, lists, quotes).
"""

import html
import re
import sys
from pathlib import Path

from reportlab.lib import colors
from reportlab.lib.enums import TA_LEFT
from reportlab.lib.pagesizes import A4
from reportlab.lib.styles import ParagraphStyle
from reportlab.lib.units import mm
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.platypus import (
    BaseDocTemplate,
    Frame,
    HRFlowable,
    KeepTogether,
    PageBreak,
    PageTemplate,
    Paragraph,
    Spacer,
    Table,
    TableStyle,
)

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "docs" / "whitepaper-zh.md"
OUT = ROOT / "docs" / "whitepaper-zh.pdf"

pdfmetrics.registerFont(TTFont("CJK", "/System/Library/Fonts/STHeiti Light.ttc", subfontIndex=0))
pdfmetrics.registerFont(TTFont("CJK-Bold", "/System/Library/Fonts/STHeiti Medium.ttc", subfontIndex=0))

INK = colors.HexColor("#1a1d23")
MUTED = colors.HexColor("#5c6470")
ACCENT = colors.HexColor("#0b5fa5")
LINE = colors.HexColor("#d8dde4")
CODE_BG = colors.HexColor("#f4f6f8")
STRIPE = colors.HexColor("#f7f9fb")
QUOTE_BG = colors.HexColor("#eef4fa")

S = {
    "body": ParagraphStyle("body", fontName="CJK", fontSize=10.2, leading=16.5, textColor=INK, spaceAfter=5),
    "h1": ParagraphStyle("h1", fontName="CJK-Bold", fontSize=19, leading=26, textColor=INK, spaceBefore=18, spaceAfter=8),
    "h2": ParagraphStyle("h2", fontName="CJK-Bold", fontSize=14.5, leading=21, textColor=ACCENT, spaceBefore=14, spaceAfter=6),
    "h3": ParagraphStyle("h3", fontName="CJK-Bold", fontSize=12, leading=18, textColor=INK, spaceBefore=10, spaceAfter=4),
    "bullet": ParagraphStyle("bullet", fontName="CJK", fontSize=10.2, leading=16.5, textColor=INK, leftIndent=14, bulletIndent=4, spaceAfter=3),
    "code": ParagraphStyle("code", fontName="CJK", fontSize=8.8, leading=13.5, textColor=INK),
    "quote": ParagraphStyle("quote", fontName="CJK", fontSize=10.2, leading=16.5, textColor=MUTED),
    "tcell": ParagraphStyle("tcell", fontName="CJK", fontSize=8.9, leading=13, textColor=INK),
    "thead": ParagraphStyle("thead", fontName="CJK-Bold", fontSize=9.0, leading=13, textColor=colors.white),
    "cover_t": ParagraphStyle("cover_t", fontName="CJK-Bold", fontSize=32, leading=44, textColor=INK),
    "cover_s": ParagraphStyle("cover_s", fontName="CJK", fontSize=13, leading=22, textColor=MUTED),
    "cover_m": ParagraphStyle("cover_m", fontName="CJK", fontSize=10.5, leading=18, textColor=MUTED),
}


def inline(text: str) -> str:
    """Markdown inline -> reportlab mini-HTML."""
    # STHeiti lacks U+2713/U+2717; fold to glyphs it does have.
    text = text.replace("✓", "√").replace("✗", "×")
    out = html.escape(text, quote=False)
    out = re.sub(r"\*\*(.+?)\*\*", r'<font face="CJK-Bold">\1</font>', out)

    def code_span(m):
        body = m.group(1)
        face = "Courier" if body.isascii() else "CJK"
        return f'<font face="{face}" size="8.6" color="#0b5fa5">{body}</font>'

    out = re.sub(r"`([^`]+)`", code_span, out)
    out = re.sub(r"\[([^\]]+)\]\(([^)]+)\)", r"\1", out)  # links -> plain text
    # Single-star emphasis: STHeiti has no italic; render muted instead.
    out = re.sub(r"(?<!\w)\*([^*\n]+)\*(?!\w)", r'<font color="#5c6470">\1</font>', out)
    return out


def code_par(line: str) -> Paragraph:
    esc = html.escape(line, quote=False).replace(" ", "&nbsp;")
    face = "Courier" if line.isascii() else "CJK"
    return Paragraph(f'<font face="{face}">{esc or "&nbsp;"}</font>', S["code"])


def make_code_block(lines):
    rows = [[code_par(l)] for l in lines] or [[code_par("")]]
    t = Table(rows, colWidths=[166 * mm])
    t.setStyle(TableStyle([
        ("BACKGROUND", (0, 0), (-1, -1), CODE_BG),
        ("BOX", (0, 0), (-1, -1), 0.5, LINE),
        ("LEFTPADDING", (0, 0), (-1, -1), 8),
        ("RIGHTPADDING", (0, 0), (-1, -1), 8),
        ("TOPPADDING", (0, 0), (-1, -1), 1.2),
        ("BOTTOMPADDING", (0, 0), (-1, -1), 1.2),
    ]))
    return t


def make_table(header, body):
    ncols = len(header)
    widths = table_widths(header, body, total=166 * mm)
    rows = [[Paragraph(inline(c), S["thead"]) for c in header]]
    for r in body:
        r = (r + [""] * ncols)[:ncols]
        rows.append([Paragraph(inline(c), S["tcell"]) for c in r])
    t = Table(rows, colWidths=widths, repeatRows=1)
    style = [
        ("BACKGROUND", (0, 0), (-1, 0), ACCENT),
        ("GRID", (0, 0), (-1, -1), 0.4, LINE),
        ("VALIGN", (0, 0), (-1, -1), "TOP"),
        ("LEFTPADDING", (0, 0), (-1, -1), 5),
        ("RIGHTPADDING", (0, 0), (-1, -1), 5),
        ("TOPPADDING", (0, 0), (-1, -1), 3.5),
        ("BOTTOMPADDING", (0, 0), (-1, -1), 3.5),
    ]
    for i in range(1, len(rows)):
        if i % 2 == 0:
            style.append(("BACKGROUND", (0, i), (-1, i), STRIPE))
    t.setStyle(TableStyle(style))
    return t


def table_widths(header, body, total):
    """Column widths proportional to content weight. Floors: a column must
    at least fit its longest unbreakable ASCII token (command names like
    `route-coverage` must not wrap mid-word)."""
    ncols = len(header)
    weights, floors = [], []
    for c in range(ncols):
        w = float(len(header[c]))
        longest_token = 0
        for r in body:
            if c < len(r):
                cell = re.sub(r"`|\*\*", "", r[c])
                w = max(w, min(48, sum(2 if ord(ch) > 0x2E80 else 1 for ch in cell) / 3))
                for tok in re.split(r"[\s/、,;:()]+", cell):
                    if tok.isascii():
                        longest_token = max(longest_token, len(tok))
        weights.append(max(6.0, w))
        # ~1.9mm per Courier-8.6 char + cell padding.
        floors.append(min(56 * mm, max(16 * mm, longest_token * 1.9 * mm + 4 * mm)))
    s = sum(weights)
    widths = [max(floors[c], total * weights[c] / s) for c in range(ncols)]
    # Rescale down proportionally above the floors if we overflow.
    over = sum(widths) - total
    if over > 0:
        slack = [widths[c] - floors[c] for c in range(ncols)]
        ts = sum(slack) or 1.0
        widths = [widths[c] - over * slack[c] / ts for c in range(ncols)]
    return widths


def make_quote(markup):
    # `markup` is already inline()-processed mini-HTML; do not escape twice.
    t = Table([[Paragraph(markup, S["quote"])]], colWidths=[166 * mm])
    t.setStyle(TableStyle([
        ("BACKGROUND", (0, 0), (-1, -1), QUOTE_BG),
        ("LINEBEFORE", (0, 0), (0, -1), 2.2, ACCENT),
        ("LEFTPADDING", (0, 0), (-1, -1), 10),
        ("RIGHTPADDING", (0, 0), (-1, -1), 8),
        ("TOPPADDING", (0, 0), (-1, -1), 6),
        ("BOTTOMPADDING", (0, 0), (-1, -1), 6),
    ]))
    return t


def parse(md: str):
    flow = []
    lines = md.split("\n")
    i = 0
    first_h1_seen = False
    while i < len(lines):
        line = lines[i]
        if line.startswith("```"):
            j = i + 1
            block = []
            while j < len(lines) and not lines[j].startswith("```"):
                block.append(lines[j])
                j += 1
            flow.append(Spacer(1, 3))
            flow.append(make_code_block(block))
            flow.append(Spacer(1, 5))
            i = j + 1
            continue
        if line.startswith("|") and i + 1 < len(lines) and re.match(r"^\|[\s\-:|]+\|?\s*$", lines[i + 1]):
            header = [c.strip() for c in line.strip().strip("|").split("|")]
            j = i + 2
            body = []
            while j < len(lines) and lines[j].startswith("|"):
                body.append([c.strip() for c in lines[j].strip().strip("|").split("|")])
                j += 1
            flow.append(Spacer(1, 3))
            flow.append(make_table(header, body))
            flow.append(Spacer(1, 6))
            i = j
            continue
        if line.startswith("# "):
            if first_h1_seen:
                flow.append(Paragraph(inline(line[2:]), S["h1"]))
            first_h1_seen = True
            i += 1
            continue
        if line.startswith("## "):
            flow.append(KeepTogether([
                Paragraph(inline(line[3:]), S["h2"]),
                HRFlowable(width="100%", thickness=0.7, color=LINE, spaceAfter=6),
            ]))
            i += 1
            continue
        if line.startswith("### "):
            flow.append(Paragraph(inline(line[4:]), S["h3"]))
            i += 1
            continue
        if line.startswith("> "):
            quote = [line[2:]]
            j = i + 1
            while j < len(lines) and lines[j].startswith(">"):
                quote.append(lines[j].lstrip("> "))
                j += 1
            text = "<br/>".join(inline(q) for q in quote if q.strip())
            flow.append(make_quote(text))
            flow.append(Spacer(1, 5))
            i = j
            continue
        if re.match(r"^\s*[-*] ", line):
            text = re.sub(r"^\s*[-*] ", "", line)
            flow.append(Paragraph(inline(text), S["bullet"], bulletText="•"))
            i += 1
            continue
        m = re.match(r"^\s*(\d+)\. (.*)", line)
        if m:
            flow.append(Paragraph(inline(m.group(2)), S["bullet"], bulletText=f"{m.group(1)}."))
            i += 1
            continue
        if line.strip() == "---":
            i += 1
            continue
        if line.strip():
            flow.append(Paragraph(inline(line), S["body"]))
        i += 1
    return flow


def footer(canvas, doc):
    canvas.saveState()
    canvas.setFont("CJK", 8)
    canvas.setFillColor(MUTED)
    canvas.drawString(24 * mm, 12 * mm, "GroundGraph 白皮书")
    canvas.drawRightString(A4[0] - 24 * mm, 12 * mm, f"第 {doc.page} 页")
    canvas.setStrokeColor(LINE)
    canvas.setLineWidth(0.5)
    canvas.line(24 * mm, 16 * mm, A4[0] - 24 * mm, 16 * mm)
    canvas.restoreState()


def cover():
    return [
        Spacer(1, 55 * mm),
        Paragraph("GroundGraph 白皮书", S["cover_t"]),
        Spacer(1, 6 * mm),
        Paragraph("非侵入式 AI 编码意图层", S["cover_s"]),
        Paragraph("用证据图回答:这段代码是干什么的,谁能证明。", S["cover_s"]),
        Spacer(1, 14 * mm),
        HRFlowable(width="38%", thickness=1.2, color=ACCENT, hAlign="LEFT"),
        Spacer(1, 8 * mm),
        Paragraph("功能全景 · 设计哲学 · 算法要点 · 质量体系 · 路线评估", S["cover_m"]),
        Spacer(1, 3 * mm),
        Paragraph("2026 年 6 月 · 与仓库同步演进", S["cover_m"]),
        PageBreak(),
    ]


def main():
    md = SRC.read_text(encoding="utf-8")
    doc = BaseDocTemplate(
        str(OUT), pagesize=A4,
        leftMargin=24 * mm, rightMargin=24 * mm, topMargin=20 * mm, bottomMargin=22 * mm,
        title="GroundGraph 白皮书", author="GroundGraph",
    )
    frame = Frame(doc.leftMargin, doc.bottomMargin, doc.width, doc.height, id="main")
    doc.addPageTemplates([PageTemplate(id="page", frames=[frame], onPage=footer)])
    doc.build(cover() + parse(md))
    print(f"wrote {OUT} ({OUT.stat().st_size // 1024} KB)")


if __name__ == "__main__":
    sys.exit(main())
