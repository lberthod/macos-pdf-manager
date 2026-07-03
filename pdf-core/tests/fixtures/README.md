# Fixtures de test

- `minimal.pdf` — PDF minimal fait main (1 page vide), xref classique, offsets calculés par script Python (voir historique git).
- `multipage_classic_xref.pdf` — document 5 pages généré avec `reportlab`, re-sauvegardé avec `pikepdf` en forçant une xref classique (`ObjectStreamMode.disable`, `qdf=True`).
- `multipage_xref_stream.pdf` — même contenu, re-sauvegardé avec `pikepdf` en forçant un cross-reference stream + object streams (`ObjectStreamMode.generate`), représentatif des PDF produits par les outils modernes (PDF 1.5+).
- `corrupted_missing_xref.pdf` — `multipage_classic_xref.pdf` tronqué juste avant sa table xref finale, pour exercer la reconstruction par balayage (`xref::reconstruct_by_scan`) et la détection de secours du catalogue.

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
```

Ce corpus reste modeste (4 fichiers) : loin du « plusieurs centaines de PDF variés » visé par le critère de sortie de la Phase 1 (architecture.md §9). Un corpus plus large (PDF scannés, formulaires AcroForm, PDF chiffrés, CJK, PDF/A...) reste à constituer — voir sprint.md.
