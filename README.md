# PDFExtract

HTTP service that turns a PDF into structured Markdown with [PdfCraft](https://github.com/storytold/pdfcraft). Send a file or a URL. Headings, emphasis and tables are kept. Images in the PDF are not copied into the Markdown. When the PDF is a scan or a set of pictures with no text layer, and `OPENROUTER_API_KEY` is set, the service transcribes the page images the same way AngelOS does and sets `vision_fallback` to true. Without that key, the response says the file is image-based and leaves the words out.

Other applications call `POST /v1/convert` with `Authorization: Bearer $PDFEXTRACT_API_KEY`. The contract is `GET /openapi.json`. A browser page on `/` uses the same endpoint.

## Request

Multipart form fields `file` or `url`, and an optional `password`:

```bash
curl -sS -H "Authorization: Bearer $PDFEXTRACT_API_KEY" \
  -F "file=@report.pdf" \
  https://your-service.up.railway.app/v1/convert
```

JSON for a URL:

```bash
curl -sS -H "Authorization: Bearer $PDFEXTRACT_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com/report.pdf"}' \
  https://your-service.up.railway.app/v1/convert
```

`?format=markdown` or `Accept: text/markdown` returns the document itself. The `X-Image-Based` header is `true` or `false`. The default JSON body is:

- `markdown`
- `title`
- `pages`
- `truncated_pages` (zero-based page indexes PdfCraft could not finish)
- `image_based`
- `vision_fallback` (true when the words were read from page images)
- `notice` (present when the PDF is image-based)

PDFs are limited to 32 MB. URLs must be `http` or `https` and must resolve to a public address.

## Configuration

| Variable | Purpose |
| --- | --- |
| `PDFEXTRACT_API_KEY` | Shared secret. Conversion is refused when this is unset. |
| `OPENROUTER_API_KEY` | Reads words from image-only PDFs. Image PDFs keep the scan notice when this is unset. |
| `PORT` | Listen port. Railway sets this. |

The image builds PdfCraft at commit `271e479c53723b1b294c866d5cb2f27604601b29` and links this service to its Markdown export.
