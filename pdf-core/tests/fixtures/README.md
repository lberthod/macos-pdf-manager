# Fixtures de test

- `minimal.pdf` — PDF minimal fait main (1 page vide), xref classique, offsets calculés par script Python (voir historique git).
- `multipage_classic_xref.pdf` — document 5 pages généré avec `reportlab`, re-sauvegardé avec `pikepdf` en forçant une xref classique (`ObjectStreamMode.disable`, `qdf=True`).
- `multipage_xref_stream.pdf` — même contenu, re-sauvegardé avec `pikepdf` en forçant un cross-reference stream + object streams (`ObjectStreamMode.generate`), représentatif des PDF produits par les outils modernes (PDF 1.5+).
- `corrupted_missing_xref.pdf` — `multipage_classic_xref.pdf` tronqué juste avant sa table xref finale, pour exercer la reconstruction par balayage (`xref::reconstruct_by_scan`) et la détection de secours du catalogue.
- `embedded_truetype_font.pdf` — texte "AVIL" en police Monaco **intégrée** (`/FontFile2`, sous-ensemble), générée via `reportlab.pdfbase.ttfonts.TTFont` puis re-sauvegardée en xref classique avec `pikepdf`. Sert à tester l'extraction de contours réels (`font.rs::glyph_outline`) : ce sous-ensemble n'embarque qu'un `cmap` Macintosh (1,0), pas de table Unicode, ce qui exerce le repli par code brut.
- `image_jpeg.pdf` — texte + photo JPEG intégrée (dégradé RGB synthétique généré via Pillow, 120×80), insérée avec `reportlab.Canvas.drawImage` puis re-sauvegardée en xref classique avec `pikepdf`. Le flux résultant chaîne `ASCII85Decode` + `DCTDecode` (comportement par défaut de reportlab), ce qui exerce la chaîne de filtres complète en plus du décodeur JPEG lui-même (`filters.rs::dct_decode`, `image.rs::decode_image`).
- `embedded_cff_font.pdf` — texte "ABC" en police STIX (`STIXGeneral.otf`, système macOS) intégrée en **CFF/Type1C** (`/FontFile3`, sous-ensemble de 3 glyphes extrait via `fonttools subset` puis sa table `CFF ` brute isolée). Construit **à la main avec pikepdf** (`Dictionary`/`Stream` directs) plutôt qu'avec reportlab, qui n'a pas de support intégré pour produire ce mode d'embarquement. Sert à tester `font.rs::glyph_outline` sur le chemin `ttf_parser::cff::Table` (CFF brut, sans conteneur OpenType).
- `image_smask.pdf` — rectangle bleu opaque recouvert d'un carré rouge cramoisi **semi-transparent** (`/SMask`, alpha uniforme ~128/255), généré via une image RGBA Pillow insérée avec `reportlab.Canvas.drawImage(..., mask='auto')` (c'est ce paramètre qui déclenche l'extraction de l'alpha en `/SMask` séparé plutôt que de l'aplatir). Sert à tester `image.rs::apply_soft_mask` et la prémultiplication dans `pdf-render`.
- `rotated_page.pdf` — page avec `/Rotate 90`, générée avec `reportlab` puis `pikepdf` (`pdf.pages[0].Rotate = 90`). Sert à tester l'application de la rotation au rendu (`pdf-render::render_page_rotated`) : a mis en évidence que `/Rotate` était parsé (`Page::rotate`) mais jamais appliqué avant ce fixture.
- `acroform_textfield.pdf` — page avec un champ de formulaire texte simple (`reportlab.Canvas.acroForm.textfield`). Sert à vérifier que la présence d'un `/AcroForm` n'empêche pas l'ouverture ni l'extraction du texte de la page (le remplissage de formulaire lui-même n'est pas implémenté, voir `pdf-edit`).
- `encrypted_rc4.pdf` — nom historique trompeur (voir la note de correction ci-dessous) : `pikepdf.Encryption(owner=..., user="", R=4)` sans `aes=False` explicite produit en réalité un chiffrement **AES-128** (`/V 4 /R 4`, `/CFM AESV2`), pas RC4 40 bits. Mot de passe utilisateur vide. Sert de fixture de bout en bout pour le déchiffrement `/Encrypt` (Sprint 22, `pdf-core::crypt`) : `Document::open` déchiffre avec succès (Algorithme 2, ISO 32000-1 §7.6.3.3 + AES-128-CBC), le texte du contenu ("Encrypted PDF test - if you can read this, decryption worked") est recomposé correctement. **Note de correction (Sprint 22) :** avant cette passe, ce fixture servait à vérifier que `Document::open` échouait proprement (`PdfError::Encrypted`) faute de déchiffrement implémenté — en implémentant ce dernier, l'inspection réelle du dictionnaire `/Encrypt` a révélé que ce fixture n'était de toute façon pas du RC4, corrigeant une description erronée présente ici depuis sa création.
- `cjk_text.pdf` — texte chinois simplifié (`你好，世界`) en police Songti **intégrée** (`/System/Library/Fonts/Supplemental/Songti.ttc`, choisie parce qu'elle a des contours TrueType `glyf` — la plupart des polices CJK système macOS sont CFF/OTF, non supportées par l'embarqueur TrueType de reportlab). Le glyphe se rend correctement (contour résolu via le `cmap` de la police) **et** le texte est intégralement récupérable via le CMap `/ToUnicode` que reportlab embarque par défaut pour ce type de sous-ensemble (`font.rs::parse_to_unicode_cmap`) — sert de fixture de non-régression pour ce parseur.
- `large_60_pages.pdf` — document de 60 pages (texte + rectangle par page, cross-reference stream + object streams), pour les tests de navigation/recherche/miniatures sur un document de taille non triviale.
- `outline_test.pdf` — 4 pages, une table des matières plate ("Section 1".."Section 4", une par page), générée avec `reportlab.Canvas.bookmarkPage`/`addOutlineEntry`. Sert à tester la lecture de `/Outlines` (`pdf-core::outline`), en particulier la résolution des destinations directes (`/Dest` tableau `[page /Fit]`) en index de page.
- `type0_cid_truetype.pdf` — texte "AB" en police composite **`/Type0`/`CIDFontType2`** (`/Encoding /Identity-H`, `/CIDToGIDMap /Identity`) : sous-ensemble TrueType Monaco réel (2 glyphes) extrait via `fonttools subset --text="AB"`, dont les GID (renumérotés par le sous-ensembleur) servent directement de codes 2 octets dans le flux de contenu (`<0001 0002>` — cas réel le plus courant, où le CID est le GID directement plutôt que passer par un `/CIDToGIDMap` explicite). Construit **à la main avec pikepdf** (comme `embedded_cff_font.pdf`), avec un `/ToUnicode` (`beginbfchar` GID -> `A`/`B`) et un `/W` donnant une largeur distincte à chaque GID (`/DW` sert de repli). Premier fixture réel `/Type0` du corpus (les autres tests composites de `pdf-core::font`/`interp` restent synthétiques) — sert de non-régression bout en bout pour `font.rs::cid_glyph_outline`/`cid_metrics` et `interp::show_text` sur un vrai PDF produit par un outil tiers.
- `acroform_checkbox.pdf` — page avec une case à cocher AcroForm simple (`reportlab.Canvas.acroForm.checkbox`, `/FT /Btn`, `Ff 2` — pas de bit `Pushbutton`/`Radio`, un seul widget qui est aussi le champ). `/AP /N` est un dictionnaire d'états (`/Off`, `/Yes`) plutôt qu'un flux unique, `/AS /Off` initialement. Sert de fixture de bout en bout pour `pdf-edit::EditSession::checkbox_fields`/`set_checkbox_field_value` (Sprint 52, #43 suite) : coche/décoche en ne touchant que `/AS`+`/V`, sans régénérer `/AP` (contrairement au champ texte, dont l'apparence est synthétisée depuis zéro).
- `acroform_radio.pdf` — page avec un groupe de 2 boutons radio AcroForm (`reportlab.Canvas.acroForm.radio`, deux appels avec le même `name`, options `"red"` sélectionnée initialement et `"blue"`). Le champ parent (`/FT /Btn`, `Ff` avec le bit `Radio` posé) n'a pas de `/Rect` propre, seuls ses deux `/Kids` (les widgets) en ont un — chacun avec son propre `/AP /N` (`/Off` + son propre nom d'état, `"red"`/`"blue"`) et son propre `/AS`. Sert de fixture de bout en bout pour `pdf-edit::EditSession::radio_groups`/`set_radio_group_value` (Sprint 53, #43 suite) : bascule `/AS` de chaque widget-enfant (un seul reste coché) et `/V` du champ parent, sans régénérer aucune apparence.
- `acroform_choice.pdf` — page avec un champ liste/menu déroulant AcroForm (`reportlab.Canvas.acroForm.choice`, `/FT /Ch`, `fieldFlags="combo"`, 3 options `[(display, code), ...]`). `/Opt` est un tableau de paires `[texte affiché, code]` mais `/V`/`/DV` valent le **premier** élément de la paire (`"Apple"`, pas `"apple"`) — comportement réel de reportlab, pas forcément la lecture qu'on ferait de l'ordre `/Opt` d'ISO 32000-1 §12.7.4.4 à la lettre. Sert de fixture de bout en bout pour `pdf-edit::EditSession::choice_fields`/`set_choice_field_value` (Sprint 54, #43 suite, dernier sous-cas) : contrairement aux cases à cocher/boutons radio, ce type de champ **régénère** son apparence (texte simple, comme un champ `/Tx`) plutôt que de basculer un `/AS` déjà présent dans `/AP /N`.
- `encrypted_user_password.pdf` — page chiffrée avec un **vrai mot de passe utilisateur non vide** (`pikepdf.Encryption(owner="ownerpass", user="secret123", R=4)`, AES-128), contrairement aux deux fixtures chiffrés existants (`encrypted_rc4.pdf`/`encrypted_aes256.pdf`, tous deux à mot de passe utilisateur vide). Sert de fixture de bout en bout pour `pdf_core::crypt::Decryptor`/`Document::open_with_password` (Sprint 58, `audit50quest.md` #50 suite) : bon mot de passe (`"secret123"`) déchiffre et recompose le texte en clair, mauvais mot de passe **et** mot de passe vide implicite tous deux rejetés avec `PdfError::IncorrectPassword`.
- `signed_document.pdf` — page avec une signature numérique `/Sig` réelle (`pyHanko`, PKCS#7/CMS détaché `adbe.pkcs7.detached`, RSA-2048/SHA-256), signée avec un certificat **auto-signé** généré pour ce fixture (`CN=Test Signer`, pas de chaîne de confiance — ce module ne la vérifie de toute façon pas, voir `signature.rs`). Généré avec un outil distinct de `reportlab`/`pikepdf` (aucun des deux ne sait signer) — voir la section dédiée ci-dessous plutôt que le bloc de régénération commun. Sert de fixture de bout en bout pour `pdf_core::signature::Document::signature_fields`/`SignatureField::verify` (Sprint 59, `sprint.md` Sprint 23+) : signature intacte acceptée (`SignatureStatus::Valid`), et falsifier un octet dans la zone couverte par `/ByteRange` **après** signature est bien détecté (`SignatureStatus::ContentModified`) — le test qui compte le plus pour ce module.

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

# rotated_page.pdf
buf6 = io.BytesIO()
c6 = canvas.Canvas(buf6, pagesize=letter)
c6.drawString(72, 720, "Rotated page test")
c6.rect(72, 600, 200, 100)
c6.showPage()
c6.save()
with Pdf.open(io.BytesIO(buf6.getvalue())) as pdf:
    pdf.pages[0].Rotate = 90
    pdf.save("rotated_page.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# acroform_textfield.pdf
buf7 = io.BytesIO()
c7 = canvas.Canvas(buf7, pagesize=letter)
c7.drawString(72, 750, "Simple AcroForm test")
c7.acroForm.textfield(name="name_field", tooltip="Your name", x=72, y=650, width=200, height=20, borderStyle="inset", forceBorder=True)
c7.showPage()
c7.save()
with Pdf.open(io.BytesIO(buf7.getvalue())) as pdf:
    pdf.save("acroform_textfield.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# encrypted_rc4.pdf
buf8 = io.BytesIO()
c8 = canvas.Canvas(buf8, pagesize=letter)
c8.drawString(72, 720, "Encrypted PDF test - if you can read this, decryption worked")
c8.showPage()
c8.save()
with Pdf.open(io.BytesIO(buf8.getvalue())) as pdf:
    pdf.save("encrypted_rc4.pdf", encryption=pikepdf.Encryption(owner="ownerpass", user="", R=4))

# cjk_text.pdf (police Songti : glyf TrueType, contrairement à la plupart
# des polices CJK système macOS qui sont CFF/OTF et non supportées par
# l'embarqueur TrueType de reportlab)
pdfmetrics.registerFont(TTFont("Songti", "/System/Library/Fonts/Supplemental/Songti.ttc", subfontIndex=0))
buf9 = io.BytesIO()
c9 = canvas.Canvas(buf9, pagesize=letter)
c9.setFont("Songti", 24)
c9.drawString(72, 700, u"你好，世界")
c9.showPage()
c9.save()
with Pdf.open(io.BytesIO(buf9.getvalue())) as pdf:
    pdf.save("cjk_text.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# large_60_pages.pdf
buf10 = io.BytesIO()
c10 = canvas.Canvas(buf10, pagesize=letter)
for i in range(60):
    c10.drawString(72, 720, f"Page {i+1} of 60 - large document stress test")
    c10.rect(72, 600, 200, 100)
    c10.showPage()
c10.save()
with Pdf.open(io.BytesIO(buf10.getvalue())) as pdf:
    pdf.save("large_60_pages.pdf", object_stream_mode=ObjectStreamMode.generate, static_id=True)

# outline_test.pdf
buf11 = io.BytesIO()
c11 = canvas.Canvas(buf11, pagesize=letter)
for i in range(4):
    c11.bookmarkPage(f"page{i}")
    c11.addOutlineEntry(f"Section {i+1}", f"page{i}", level=0)
    c11.drawString(72, 720, f"Page {i+1} of outline test")
    c11.showPage()
c11.save()
with Pdf.open(io.BytesIO(buf11.getvalue())) as pdf:
    pdf.save("outline_test.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# type0_cid_truetype.pdf (nécessite aussi fonttools : pip install fonttools)
# 1) sous-ensembler une police TrueType (glyf) en ligne de commande :
#    fonttools subset /System/Library/Fonts/Monaco.ttf --text="AB" \
#      --output-file=/tmp/monaco_subset.ttf --no-layout-closure
# 2) lire les octets bruts + les GID (renumérotés par le sous-ensembleur) des glyphes retenus :
from fontTools.ttLib import TTFont as _TTFont
with open("/tmp/monaco_subset.ttf", "rb") as f:
    truetype_bytes = f.read()
subset_font = _TTFont("/tmp/monaco_subset.ttf")
cmap = subset_font.getBestCmap()
gid_a = subset_font.getGlyphID(cmap[ord("A")])
gid_b = subset_font.getGlyphID(cmap[ord("B")])

pdf12 = pikepdf.new()
page12 = pdf12.add_blank_page(page_size=(400, 200))
font_file2 = pikepdf.Stream(pdf12, truetype_bytes)
descriptor12 = pdf12.make_indirect(pikepdf.Dictionary(
    Type=pikepdf.Name.FontDescriptor, FontName=pikepdf.Name("/Monaco-Identity-H"), Flags=32,
    FontBBox=pikepdf.Array([-100, -200, 1000, 900]), ItalicAngle=0, Ascent=900,
    Descent=-200, CapHeight=700, StemV=80, FontFile2=font_file2,
))
cid_font12 = pdf12.make_indirect(pikepdf.Dictionary(
    Type=pikepdf.Name.Font, Subtype=pikepdf.Name.CIDFontType2, BaseFont=pikepdf.Name("/Monaco-Identity-H"),
    CIDSystemInfo=pikepdf.Dictionary(Registry="Adobe", Ordering="Identity", Supplement=0),
    FontDescriptor=descriptor12, DW=600,
    # /CIDToGIDMap /Identity : le code du flux de contenu (2 octets) EST le GID directement.
    W=pikepdf.Array([gid_a, pikepdf.Array([650]), gid_b, pikepdf.Array([700])]),
    CIDToGIDMap=pikepdf.Name.Identity,
))
to_unicode12 = (
    "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n"
    "/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n"
    "1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n"
    "2 beginbfchar\n"
    f"<{gid_a:04X}> <0041>\n<{gid_b:04X}> <0042>\n"
    "endbfchar\nendcmap\nend\nend\n"
).encode("ascii")
font12 = pdf12.make_indirect(pikepdf.Dictionary(
    Type=pikepdf.Name.Font, Subtype=pikepdf.Name.Type0, BaseFont=pikepdf.Name("/Monaco-Identity-H"),
    Encoding=pikepdf.Name("/Identity-H"), DescendantFonts=pikepdf.Array([cid_font12]),
    ToUnicode=pikepdf.Stream(pdf12, to_unicode12),
))
page12.Resources = pikepdf.Dictionary(Font=pikepdf.Dictionary(F1=font12))
page12.Contents = pikepdf.Stream(pdf12, f"BT /F1 48 Tf 50 100 Td <{gid_a:04X}{gid_b:04X}> Tj ET".encode("ascii"))
pdf12.save("type0_cid_truetype.pdf")

# acroform_checkbox.pdf
buf13 = io.BytesIO()
c13 = canvas.Canvas(buf13, pagesize=letter)
c13.drawString(72, 750, "Simple AcroForm checkbox test")
c13.acroForm.checkbox(name="agree_field", tooltip="Agree?", x=72, y=650, size=16, buttonStyle="check", borderStyle="inset", checked=False)
c13.showPage()
c13.save()
with Pdf.open(io.BytesIO(buf13.getvalue())) as pdf:
    pdf.save("acroform_checkbox.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# acroform_radio.pdf
buf14 = io.BytesIO()
c14 = canvas.Canvas(buf14, pagesize=letter)
c14.drawString(72, 750, "Simple AcroForm radio group test")
c14.acroForm.radio(name="color_choice", value="red", selected=True, x=72, y=680, size=16, buttonStyle="check", borderStyle="inset")
c14.drawString(96, 680, "Red")
c14.acroForm.radio(name="color_choice", value="blue", selected=False, x=72, y=650, size=16, buttonStyle="check", borderStyle="inset")
c14.drawString(96, 650, "Blue")
c14.showPage()
c14.save()
with Pdf.open(io.BytesIO(buf14.getvalue())) as pdf:
    pdf.save("acroform_radio.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# acroform_choice.pdf
buf15 = io.BytesIO()
c15 = canvas.Canvas(buf15, pagesize=letter)
c15.drawString(72, 750, "Simple AcroForm choice field test")
c15.acroForm.choice(
    name="fruit_choice",
    value="apple",
    options=[("apple", "Apple"), ("banana", "Banana"), ("cherry", "Cherry")],
    x=72, y=650, width=150, height=20,
    fieldFlags="combo",
    borderStyle="inset",
)
c15.showPage()
c15.save()
with Pdf.open(io.BytesIO(buf15.getvalue())) as pdf:
    pdf.save("acroform_choice.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# encrypted_user_password.pdf
buf16 = io.BytesIO()
c16 = canvas.Canvas(buf16, pagesize=letter)
c16.drawString(72, 720, "Password protected PDF test - if you can read this, the password worked")
c16.showPage()
c16.save()
with Pdf.open(io.BytesIO(buf16.getvalue())) as pdf:
    pdf.save("encrypted_user_password.pdf", encryption=pikepdf.Encryption(owner="ownerpass", user="secret123", R=4))
```

- `bold_italic_standard_fonts.pdf` — cinq lignes en polices standard non embarquées **hors Helvetica plain** (`Times-Bold`, `Times-Italic`, `Courier-BoldOblique`, `Helvetica-BoldOblique`, `Symbol`) : exerce la sélection de face gras/italique de la substitution système (`.ttc` avec choix de face) au-delà du seul fixture Helvetica déjà couvert.
- `landscape_mixed_page_sizes.pdf` — 3 pages de tailles/orientations différentes (Letter portrait, A4 paysage, carré 300×300), générées avec `reportlab.Canvas.setPageSize` entre chaque page. Sert de fixture de non-régression pour la limitation connue du défilement continu de `pdf-ui` (hauteur de ligne dérivée de la page 0 uniquement, voir sprint.md Sprint 9-10) et plus généralement pour vérifier que chaque page garde bien sa propre `/MediaBox`.
- `cmyk_jpeg.pdf` — photo JPEG **CMYK** (4 composantes, Pillow `Image.new("CMYK", ...)`) plutôt que RGB. A mis en évidence un vrai bug lors de la construction de ce fixture : `zune-jpeg` convertit certains JPEG CMYK/YCCK en sortie RGB (3 composantes) au lieu de préserver les 4 composantes déclarées par `/ColorSpace /DeviceCMYK`, ce qui faisait échouer silencieusement le décodage (`image.rs::decode_image` calculait une taille attendue sur la base des 4 composantes déclarées). Corrigé dans `image.rs::decode_image` : la disposition réelle des octets décodés (déduite de leur longueur) prime sur la déclaration `/ColorSpace` quand les deux divergent.
- `incremental_updates_chain.pdf` — trois mises à jour incrémentales chaînées (`/Prev -> /Prev -> /Prev`, `pikepdf` réouverture + re-sauvegarde x3), chacune *ajoutant* une ligne de texte au flux de contenu existant (`/Contents` devient un tableau) plutôt que de le remplacer — contre le seul niveau simple déjà couvert par `corrupted_missing_xref.pdf`. Sert à vérifier que la chaîne complète reste résolvable et que le contenu de chaque révision reste lisible.
- `malformed_wrong_length.pdf` — `/Length` d'un flux de contenu délibérément trop court de 10 octets (erreur d'auteurs réelle courante, différente de la xref tronquée déjà couverte par `corrupted_missing_xref.pdf`). Sert à vérifier que le parseur retrouve la fin réelle du flux via le mot-clé `endstream` plutôt que de tronquer silencieusement le contenu à la valeur (fausse) de `/Length`.
- `encrypted_aes256.pdf` — PDF chiffré **AES-256** (`pikepdf.Encryption(..., R=6, aes=True)`, `/V 5 /R 6`), complémentaire à `encrypted_rc4.pdf` (AES-128, voir sa note de correction) : sert de fixture de bout en bout pour le chemin de dérivation de clé le plus complexe (révision 6, hachage "renforcé" ISO 32000-2 Annexe C, `pdf-core::crypt::hardened_hash`) — `Document::open` déchiffre avec succès, le texte ("AES-256 encrypted PDF test") est recomposé correctement.
- `indexed_color_image.pdf` — image `/ColorSpace /Indexed /DeviceRGB` (palette 4 couleurs + échantillons 1 octet/pixel), construite à la main avec pikepdf. `/Indexed` n'est pas résolu (`image.rs::resolve_color_space`, limitation documentée) : ce fixture sert de non-régression pour la dégradation gracieuse attendue — la page s'ouvre et se rend sans planter, l'image apparaît dans la `DisplayList` avec `pixels: None` plutôt que de faire échouer toute la page.
- `scanned_page_like.pdf` — page pleine (850×1100) occupée entièrement par une seule image JPEG (damier synthétique en niveaux proches, Pillow), sans aucun texte ni police — reproduit la structure d'un vrai PDF scanné (image plein page, pas de couche texte) sans nécessiter `CCITTFaxDecode`/`JBIG2Decode` (hors périmètre de cette passe, voir sprint.md) : comble partiellement le manque signalé de longue date dans ce README, sur le chemin `DCTDecode` déjà supporté.
- `pdfa_like_minimal.pdf` — approximation structurelle minimale de PDF/A : `/Metadata` (flux XMP avec `pdfaid:part`/`pdfaid:conformance`) + `/OutputIntents` (`/S /GTS_PDFA1`), sans validation de conformité PDF/A complète (hors périmètre de cette passe). Sert à exercer le chemin `/Metadata`/`/OutputIntents`, absent du reste du corpus.
- `type0_cid_cff.pdf` — texte chinois simplifié (`你好`) en police composite **`/Type0`/`CIDFontType0`** (CFF CID-keyed, `/FontFile3` sous-type `CIDFontType0C`) : sous-ensemble réel de Hiragino Sans GB (`/System/Library/Fonts/Hiragino Sans GB.ttc`, police système CJK, `/ROS Adobe-GB1`) extrait via `fonttools subset --font-number=0 --text="你好"`. Les codes du flux de contenu (`<07760B46>`) sont les **CID** (1914 et 2886, lus dans le charset CFF du sous-ensemble après coup, `cid01914`/`cid02886`) — pas des GID : contrairement à `type0_cid_truetype.pdf` (`/CIDFontType2`, `/CIDToGIDMap /Identity`), ce chemin résout le glyphe via le charset interne de la table CFF (`font.rs::cid_glyph_outline`, `ttf_parser::cff::Table::glyph_cid` inversé), sans jamais consulter `/CIDToGIDMap`. Construit à la main avec pikepdf (comme `embedded_cff_font.pdf`/`type0_cid_truetype.pdf`), avec `/ToUnicode` et `/W` par CID. Comble le manque signalé dans sprint.md (Sprint 7-8) : premier fixture réel `/CIDFontType0` du corpus (les tests correspondants dans `font.rs` restaient synthétiques). Validé par rendu PNG (724 pixels non blancs) et `pdf-cli text` (`"你好"` recomposé exactement).

Ce corpus compte désormais 25 fichiers : toujours loin du « plusieurs centaines de PDF variés » au sens littéral du critère de sortie de la Phase 1 (architecture.md §9) — obtenir des centaines de PDF *réels* (scans, formulaires remplis en pratique, PDF/A produits par des outils tiers variés) demanderait une source externe (web, jeux de données publics) qui n'est pas accessible depuis cet environnement de développement, et ne serait de toute façon qu'une accumulation de volume, pas de diversité structurelle si elle n'est pas triée. À la place, ce corpus a été élargi en profondeur : il couvre maintenant un représentant de chaque catégorie avancée citée par le critère (rotation, formulaire, chiffrement RC4 **et** AES-256, CJK avec police composite `/CIDFontType0` **et** `/CIDFontType2`, document de taille non triviale, table des matières, tailles de page hétérogènes, chaîne de mises à jour incrémentales à 3 niveaux, corruption de flux par `/Length` erroné, espace colorimétrique non supporté avec dégradation gracieuse, image CMYK, page pleine image façon scan, métadonnées PDF/A-like) plutôt qu'une seule variante par grande catégorie. Chaque fixture visuel est aussi désormais comparé pixel par pixel à une image de référence (`pdf-render/tests/golden.rs` + `pdf-render-gpu/tests/cross_backend.rs`, voir ces fichiers) plutôt que seulement vérifié "ça n'a pas planté" — ce qui répond au deuxième trou identifié dès le Sprint 0 (harnais de comparaison d'images). Un vrai corpus de volume avec des scans/PDF/A authentiques reste une tâche distincte, à mener avec un accès à des jeux de données externes.

Régénération des 10 fixtures les plus récentes (nécessite le même venv que ci-dessus) :

```python
# bold_italic_standard_fonts.pdf
buf = io.BytesIO()
c = canvas.Canvas(buf, pagesize=letter)
c.setFont("Times-Bold", 24); c.drawString(72, 740, "Times-Bold heading")
c.setFont("Times-Italic", 18); c.drawString(72, 710, "Times-Italic subheading")
c.setFont("Courier-BoldOblique", 16); c.drawString(72, 680, "Courier-BoldOblique code")
c.setFont("Helvetica-BoldOblique", 16); c.drawString(72, 650, "Helvetica-BoldOblique emphasis")
c.setFont("Symbol", 20); c.drawString(72, 620, "abgd")
c.showPage(); c.save()
with Pdf.open(io.BytesIO(buf.getvalue())) as pdf:
    pdf.save("bold_italic_standard_fonts.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# landscape_mixed_page_sizes.pdf (Letter -> A4 paysage -> carré)
from reportlab.lib.pagesizes import A4, landscape
buf = io.BytesIO()
c = canvas.Canvas(buf, pagesize=letter)
c.drawString(72, 720, "Page 1 - Letter portrait"); c.rect(72, 600, 200, 100); c.showPage()
c.setPageSize(landscape(A4)); c.drawString(72, 400, "Page 2 - A4 landscape"); c.rect(72, 300, 200, 80); c.showPage()
c.setPageSize((300, 300)); c.drawString(30, 260, "Page 3 - square"); c.rect(30, 100, 150, 100); c.showPage()
c.save()
with Pdf.open(io.BytesIO(buf.getvalue())) as pdf:
    pdf.save("landscape_mixed_page_sizes.pdf", object_stream_mode=ObjectStreamMode.generate, static_id=True)

# cmyk_jpeg.pdf (nécessite aussi Pillow)
img = Image.new("CMYK", (100, 80))
px = img.load()
for y in range(80):
    for x in range(100):
        px[x, y] = (int(255 * x / 99), int(255 * y / 79), 40, 10)
jpeg_buf = io.BytesIO(); img.save(jpeg_buf, format="JPEG", quality=90); jpeg_buf.seek(0)
buf = io.BytesIO()
c = canvas.Canvas(buf, pagesize=letter)
c.drawString(72, 750, "CMYK JPEG test")
c.drawImage(ImageReader(jpeg_buf), 72, 600, width=200, height=160)
c.showPage(); c.save()
with Pdf.open(io.BytesIO(buf.getvalue())) as pdf:
    pdf.save("cmyk_jpeg.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# incremental_updates_chain.pdf (3 sauvegardes incrémentales chaînées)
buf = io.BytesIO()
c = canvas.Canvas(buf, pagesize=letter)
c.drawString(72, 720, "Revision 1"); c.showPage(); c.save()
data = buf.getvalue()
for i in range(2, 5):
    with Pdf.open(io.BytesIO(data)) as pdf:
        page = pdf.pages[0]
        content = f"BT /F1 24 Tf 72 {680 - i * 20} Td (Revision {i}) Tj ET".encode("ascii")
        new_stream = pikepdf.Stream(pdf, content)
        existing = page.Contents
        page.Contents = (pikepdf.Array([existing, new_stream]) if not isinstance(existing, pikepdf.Array)
                          else pikepdf.Array(list(existing) + [new_stream]))
        out = io.BytesIO()
        pdf.save(out, object_stream_mode=ObjectStreamMode.disable, linearize=False)
        data = out.getvalue()
with open("incremental_updates_chain.pdf", "wb") as f:
    f.write(data)

# malformed_wrong_length.pdf (base sauvegardée en qdf=True, /Length raccourci de 10)
import re
with open("_wrong_length_base.pdf", "rb") as f:
    raw = bytearray(f.read())
m = re.search(rb"/Length (\d+)", raw)
new_len = max(int(m.group(1)) - 10, 1)
raw = raw[: m.start(1)] + str(new_len).encode() + raw[m.end(1):]
with open("malformed_wrong_length.pdf", "wb") as f:
    f.write(raw)

# encrypted_aes256.pdf
buf = io.BytesIO()
c = canvas.Canvas(buf, pagesize=letter)
c.drawString(72, 720, "AES-256 encrypted PDF test"); c.showPage(); c.save()
with Pdf.open(io.BytesIO(buf.getvalue())) as pdf:
    pdf.save("encrypted_aes256.pdf", encryption=pikepdf.Encryption(owner="ownerpass", user="", R=6, aes=True))

# indexed_color_image.pdf
pdf = pikepdf.new()
page = pdf.add_blank_page(page_size=(200, 200))
palette = bytes([0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255])
width, height = 50, 50
samples = bytes([(x // 13 + y // 13) % 4 for y in range(height) for x in range(width)])
image_stream = pikepdf.Stream(pdf, samples)
image_stream.Type = pikepdf.Name.XObject; image_stream.Subtype = pikepdf.Name.Image
image_stream.Width = width; image_stream.Height = height; image_stream.BitsPerComponent = 8
image_stream.ColorSpace = pikepdf.Array([
    pikepdf.Name.Indexed, pikepdf.Name.DeviceRGB, 3, pikepdf.String(palette.decode("latin1"))
])
page.Resources = pikepdf.Dictionary(XObject=pikepdf.Dictionary(Im0=image_stream))
page.Contents = pikepdf.Stream(pdf, b"q 150 0 0 150 25 25 cm /Im0 Do Q")
pdf.save("indexed_color_image.pdf")

# scanned_page_like.pdf
img = Image.new("RGB", (850, 1100))
px = img.load()
for y in range(1100):
    for x in range(850):
        px[x, y] = (245, 245, 240) if (x // 40 + y // 40) % 2 == 0 else (238, 238, 230)
jpeg_buf = io.BytesIO(); img.save(jpeg_buf, format="JPEG", quality=80); jpeg_buf.seek(0)
buf = io.BytesIO()
c = canvas.Canvas(buf, pagesize=(850, 1100))
c.drawImage(ImageReader(jpeg_buf), 0, 0, width=850, height=1100)
c.showPage(); c.save()
with Pdf.open(io.BytesIO(buf.getvalue())) as pdf:
    pdf.save("scanned_page_like.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# pdfa_like_minimal.pdf
buf = io.BytesIO()
c = canvas.Canvas(buf, pagesize=letter)
c.drawString(72, 720, "PDF/A-like metadata test"); c.showPage(); c.save()
with Pdf.open(io.BytesIO(buf.getvalue())) as pdf:
    xmp = b"""<?xpacket begin="\xef\xbb\xbf" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/">
   <pdfaid:part>1</pdfaid:part>
   <pdfaid:conformance>B</pdfaid:conformance>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"""
    meta_stream = pdf.make_indirect(pikepdf.Stream(pdf, xmp))
    meta_stream.Type = pikepdf.Name.Metadata; meta_stream.Subtype = pikepdf.Name.XML
    pdf.Root.Metadata = meta_stream
    output_intent = pdf.make_indirect(pikepdf.Dictionary(
        Type=pikepdf.Name.OutputIntent, S=pikepdf.Name.GTS_PDFA1,
        OutputConditionIdentifier=pikepdf.String("sRGB"),
    ))
    pdf.Root.OutputIntents = pikepdf.Array([output_intent])
    pdf.save("pdfa_like_minimal.pdf", object_stream_mode=ObjectStreamMode.disable, qdf=True, static_id=True)

# type0_cid_cff.pdf (nécessite fonttools ; police système Hiragino Sans GB, CFF CID-keyed)
from fontTools.ttLib import TTFont as _TTFont2
# 1) sous-ensembler en ligne de commande :
#    python -m fontTools.subset "/System/Library/Fonts/Hiragino Sans GB.ttc" \
#      --font-number=0 --text="你好" --output-file=hiragino_gb_subset.otf --no-layout-closure
subset_font = _TTFont2("hiragino_gb_subset.otf")
cff = subset_font["CFF "].cff
charset = cff[cff.fontNames[0]].charset  # ['.notdef', 'cid01914', 'cid02886']
cid_ni, cid_hao = [int(name[3:]) for name in charset if name != ".notdef"]
cff_bytes = subset_font.reader["CFF "]

pdf = pikepdf.new()
page = pdf.add_blank_page(page_size=(400, 200))
font_file3 = pikepdf.Stream(pdf, cff_bytes)
font_file3.Subtype = pikepdf.Name.CIDFontType0C
descriptor = pdf.make_indirect(pikepdf.Dictionary(
    Type=pikepdf.Name.FontDescriptor, FontName=pikepdf.Name("/HiraginoSansGB-CID"), Flags=32,
    FontBBox=pikepdf.Array([-100, -200, 1000, 900]), ItalicAngle=0, Ascent=900,
    Descent=-200, CapHeight=700, StemV=80, FontFile3=font_file3,
))
cid_font = pdf.make_indirect(pikepdf.Dictionary(
    Type=pikepdf.Name.Font, Subtype=pikepdf.Name.CIDFontType0, BaseFont=pikepdf.Name("/HiraginoSansGB-CID"),
    CIDSystemInfo=pikepdf.Dictionary(Registry="Adobe", Ordering="GB1", Supplement=6),
    FontDescriptor=descriptor, DW=1000,
    W=pikepdf.Array([cid_ni, pikepdf.Array([980]), cid_hao, pikepdf.Array([1000])]),
))
to_unicode = (
    "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n"
    "/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n"
    "1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n2 beginbfchar\n"
    f"<{cid_ni:04X}> <4F60>\n<{cid_hao:04X}> <597D>\nendbfchar\nendcmap\nend\nend\n"
).encode("ascii")
font = pdf.make_indirect(pikepdf.Dictionary(
    Type=pikepdf.Name.Font, Subtype=pikepdf.Name.Type0, BaseFont=pikepdf.Name("/HiraginoSansGB-CID"),
    Encoding=pikepdf.Name("/Identity-H"), DescendantFonts=pikepdf.Array([cid_font]),
    ToUnicode=pikepdf.Stream(pdf, to_unicode),
))
page.Resources = pikepdf.Dictionary(Font=pikepdf.Dictionary(F1=font))
page.Contents = pikepdf.Stream(pdf, f"BT /F1 48 Tf 50 100 Td <{cid_ni:04X}{cid_hao:04X}> Tj ET".encode("ascii"))
pdf.save("type0_cid_cff.pdf")
```

### `signed_document.pdf` (nécessite en plus un venv avec `pyhanko`)

Ni `reportlab` ni `pikepdf` ne savent produire de signature numérique —
`pyhanko` (`pip install pyhanko`) le fait, plus la génération d'un
certificat de test auto-signé via `cryptography` (déjà une dépendance de
`pyhanko`) :

```python
import datetime
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID

key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
subject = issuer = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Test Signer")])
cert = (
    x509.CertificateBuilder()
    .subject_name(subject)
    .issuer_name(issuer)
    .public_key(key.public_key())
    .serial_number(x509.random_serial_number())
    .not_valid_before(datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=1))
    .not_valid_after(datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=3650))
    .sign(key, hashes.SHA256())
)
with open("signer_key.pem", "wb") as f:
    f.write(key.private_bytes(
        encoding=serialization.Encoding.PEM,
        format=serialization.PrivateFormat.TraditionalOpenSSL,
        encryption_algorithm=serialization.NoEncryption(),
    ))
with open("signer_cert.pem", "wb") as f:
    f.write(cert.public_bytes(serialization.Encoding.PEM))

# Puis signer un PDF minimal généré avec reportlab :
import io
from reportlab.pdfgen import canvas
from reportlab.lib.pagesizes import letter
from pyhanko.sign import signers
from pyhanko.sign.fields import SigFieldSpec, append_signature_field
from pyhanko.pdf_utils.incremental_writer import IncrementalPdfFileWriter

buf = io.BytesIO()
c = canvas.Canvas(buf, pagesize=letter)
c.drawString(72, 720, "Digitally signed PDF test")
c.showPage()
c.save()
buf.seek(0)

w = IncrementalPdfFileWriter(buf)
append_signature_field(w, SigFieldSpec(sig_field_name="Signature1"))
signer = signers.SimpleSigner.load("signer_key.pem", "signer_cert.pem", key_passphrase=None)
out = signers.sign_pdf(
    w,
    signers.PdfSignatureMetadata(field_name="Signature1", reason="Testing", name="Test Signer"),
    signer=signer,
)
with open("signed_document.pdf", "wb") as f:
    f.write(out.getvalue())
```
