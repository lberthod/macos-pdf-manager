# Fixtures de test

- `minimal.pdf` — PDF minimal fait main (1 page vide), xref classique, offsets calculés par script Python (voir historique git).
- `multipage_classic_xref.pdf` — document 5 pages généré avec `reportlab`, re-sauvegardé avec `pikepdf` en forçant une xref classique (`ObjectStreamMode.disable`, `qdf=True`).
- `multipage_xref_stream.pdf` — même contenu, re-sauvegardé avec `pikepdf` en forçant un cross-reference stream + object streams (`ObjectStreamMode.generate`), représentatif des PDF produits par les outils modernes (PDF 1.5+).
- `corrupted_missing_xref.pdf` — `multipage_classic_xref.pdf` tronqué juste avant sa table xref finale, pour exercer la reconstruction par balayage (`xref::reconstruct_by_scan`) et la détection de secours du catalogue.
- `embedded_truetype_font.pdf` — texte "AVIL" en police Monaco **intégrée** (`/FontFile2`, sous-ensemble), générée via `reportlab.pdfbase.ttfonts.TTFont` puis re-sauvegardée en xref classique avec `pikepdf`. Sert à tester l'extraction de contours réels (`font.rs::glyph_outline`) : ce sous-ensemble n'embarque qu'un `cmap` Macintosh (1,0), pas de table Unicode, ce qui exerce le repli par code brut.
- `image_jpeg.pdf` — texte + photo JPEG intégrée (dégradé RGB synthétique généré via Pillow, 120×80), insérée avec `reportlab.Canvas.drawImage` puis re-sauvegardée en xref classique avec `pikepdf`. Le flux résultant chaîne `ASCII85Decode` + `DCTDecode` (comportement par défaut de reportlab), ce qui exerce la chaîne de filtres complète en plus du décodeur JPEG lui-même (`filters.rs::dct_decode`, `image.rs::decode_image`).
- `embedded_cff_font.pdf` — texte "ABC" en police STIX (`STIXGeneral.otf`, système macOS) intégrée en **CFF/Type1C** (`/FontFile3`, sous-ensemble de 3 glyphes extrait via `fonttools subset` puis sa table `CFF ` brute isolée). Construit **à la main avec pikepdf** (`Dictionary`/`Stream` directs) plutôt qu'avec reportlab, qui n'a pas de support intégré pour produire ce mode d'embarquement. Sert à tester `font.rs::glyph_outline` sur le chemin `ttf_parser::cff::Table` (CFF brut, sans conteneur OpenType).
- `image_smask.pdf` — rectangle bleu opaque recouvert d'un carré rouge cramoisi **semi-transparent** (`/SMask`, alpha uniforme ~128/255), généré via une image RGBA Pillow insérée avec `reportlab.Canvas.drawImage(..., mask='auto')` (c'est ce paramètre qui déclenche l'extraction de l'alpha en `/SMask` séparé plutôt que de l'aplatir). Sert à tester `image.rs::apply_soft_mask` et la prémultiplication dans `pdf-render`.

Régénération (nécessite un venv avec `pikepdf` + `reportlab`) :

```python
from reportlab.pdfgen import canvas
from reportlab.lib.pagesizes import letter
import pikepdf
from pikepdf import Pdf, ObjectStreamMode
import io

buf = io.BytesIO()
c = canvas.Canvas(buf, pagesize=letter)
for i in range(5):
    c.drawString(72, 720, f"Page {i+1} - Hello, PDF Manager!")
    c.rect(72, 600, 200, 100)
    c.showPage()
c.save()

with Pdf.open(io.BytesIO(buf.getvalue())) as pdf:
    pdf.save("multipage_classic_xref.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)
with Pdf.open(io.BytesIO(buf.getvalue())) as pdf:
    pdf.save("multipage_xref_stream.pdf", object_stream_mode=ObjectStreamMode.generate, static_id=True)

# embedded_truetype_font.pdf
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
pdfmetrics.registerFont(TTFont("Monaco", "/System/Library/Fonts/Monaco.ttf"))
buf2 = io.BytesIO()
c2 = canvas.Canvas(buf2, pagesize=letter)
c2.setFont("Monaco", 36)
c2.drawString(72, 700, "AVIL")
c2.showPage()
c2.save()
with Pdf.open(io.BytesIO(buf2.getvalue())) as pdf:
    pdf.save("embedded_truetype_font.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# image_jpeg.pdf (nécessite aussi Pillow)
from PIL import Image
img = Image.new("RGB", (120, 80))
px = img.load()
for y in range(80):
    for x in range(120):
        px[x, y] = (int(255 * x / 119), int(255 * y / 79), 128)
jpeg_buf = io.BytesIO()
img.save(jpeg_buf, format="JPEG", quality=85)
jpeg_buf.seek(0)

buf3 = io.BytesIO()
c3 = canvas.Canvas(buf3, pagesize=letter)
c3.drawString(72, 750, "Image test page")
c3.drawImage(ImageReader(jpeg_buf), 72, 600, width=240, height=160)  # ImageReader depuis reportlab.lib.utils
c3.showPage()
c3.save()
with Pdf.open(io.BytesIO(buf3.getvalue())) as pdf:
    pdf.save("image_jpeg.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# embedded_cff_font.pdf (nécessite aussi fonttools[subset] : pip install fonttools)
# 1) sous-ensembler la police système en ligne de commande :
#    fonttools subset /System/Library/Fonts/Supplemental/STIXGeneral.otf \
#      --text="ABC" --output-file=/tmp/stix_subset.otf --no-layout-closure
# 2) extraire la table CFF brute et construire le PDF à la main :
from fontTools.ttLib import TTFont
cff_bytes = TTFont("/tmp/stix_subset.otf").reader['CFF ']

pdf4 = pikepdf.new()
page4 = pdf4.add_blank_page(page_size=(400, 200))
font_file3 = pikepdf.Stream(pdf4, cff_bytes)
font_file3.Subtype = pikepdf.Name.Type1C
descriptor = pdf4.make_indirect(pikepdf.Dictionary(
    Type=pikepdf.Name.FontDescriptor, FontName=pikepdf.Name.STIXGeneral, Flags=32,
    FontBBox=pikepdf.Array([-100, -200, 1000, 900]), ItalicAngle=0, Ascent=900,
    Descent=-200, CapHeight=700, StemV=80, MissingWidth=500, FontFile3=font_file3,
))
font = pdf4.make_indirect(pikepdf.Dictionary(
    Type=pikepdf.Name.Font, Subtype=pikepdf.Name.Type1, BaseFont=pikepdf.Name.STIXGeneral,
    FirstChar=65, LastChar=67, Widths=pikepdf.Array([700, 700, 700]),
    FontDescriptor=descriptor, Encoding=pikepdf.Name.WinAnsiEncoding,
))
page4.Resources = pikepdf.Dictionary(Font=pikepdf.Dictionary(F1=font))
page4.Contents = pikepdf.Stream(pdf4, b"BT /F1 48 Tf 50 100 Td (ABC) Tj ET")
pdf4.save("embedded_cff_font.pdf")

# image_smask.pdf (nécessite aussi Pillow)
img = Image.new("RGBA", (100, 100), (0, 0, 0, 0))
px = img.load()
for y in range(100):
    for x in range(100):
        px[x, y] = (220, 20, 60, 128)  # rouge cramoisi, alpha 128/255
img.save("/tmp/translucent.png")

from reportlab.lib.utils import ImageReader
buf5 = io.BytesIO()
c5 = canvas.Canvas(buf5, pagesize=letter)
c5.setFillColorRGB(0, 0, 1)
c5.rect(50, 600, 200, 150, fill=1, stroke=0)  # rectangle bleu opaque en dessous
c5.drawImage("/tmp/translucent.png", 100, 620, width=150, height=150, mask='auto')
c5.showPage()
c5.save()
with Pdf.open(io.BytesIO(buf5.getvalue())) as pdf:
    pdf.save("image_smask.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)
```

Ce corpus reste modeste (8 fichiers) : loin du « plusieurs centaines de PDF variés » visé par le critère de sortie de la Phase 1 (architecture.md §9). Un corpus plus large (PDF scannés, formulaires AcroForm, PDF chiffrés, CJK, PDF/A...) reste à constituer — voir sprint.md.
