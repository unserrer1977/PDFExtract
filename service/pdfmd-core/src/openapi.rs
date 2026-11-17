//! Machine-readable description of the conversion API.

pub const OPENAPI: &str = r##"{
  "openapi": "3.0.3",
  "info": {
    "title": "PDFExtract",
    "version": "0.1.0",
    "description": "Convert a PDF to structured Markdown using PdfCraft. Send a file or a URL. Images in the PDF are not copied into the Markdown. Image-only PDFs are reported to the caller."
  },
  "components": {
    "securitySchemes": {
      "bearer": {
        "type": "http",
        "scheme": "bearer"
      }
    },
    "schemas": {
      "Conversion": {
        "type": "object",
        "required": ["markdown", "pages", "truncated_pages", "image_based"],
        "properties": {
          "markdown": {"type": "string"},
          "title": {"type": "string", "nullable": true},
          "pages": {"type": "integer"},
          "truncated_pages": {"type": "array", "items": {"type": "integer"}, "description": "Zero-based page indexes PdfCraft could not read completely."},
          "image_based": {"type": "boolean"},
          "notice": {"type": "string", "nullable": true}
        }
      },
      "Error": {
        "type": "object",
        "required": ["error"],
        "properties": {"error": {"type": "string"}}
      }
    }
  },
  "paths": {
    "/health": {
      "get": {
        "summary": "Liveness",
        "responses": {"200": {"description": "The process is serving requests"}}
      }
    },
    "/v1/convert": {
      "post": {
        "summary": "Convert a PDF to Markdown",
        "security": [{"bearer": []}],
        "parameters": [
          {
            "name": "format",
            "in": "query",
            "schema": {"type": "string", "enum": ["json", "markdown"]},
            "description": "Response shape. text/markdown in Accept selects Markdown when this is omitted."
          }
        ],
        "requestBody": {
          "required": true,
          "content": {
            "multipart/form-data": {
              "schema": {
                "type": "object",
                "properties": {
                  "file": {"type": "string", "format": "binary"},
                  "url": {"type": "string"},
                  "password": {"type": "string"}
                }
              }
            },
            "application/json": {
              "schema": {
                "type": "object",
                "required": ["url"],
                "properties": {
                  "url": {"type": "string"},
                  "password": {"type": "string"}
                }
              }
            }
          }
        },
        "responses": {
          "200": {
            "description": "Markdown document",
            "headers": {
              "X-Image-Based": {"schema": {"type": "string", "enum": ["true", "false"]}}
            },
            "content": {
              "application/json": {"schema": {"$ref": "#/components/schemas/Conversion"}},
              "text/markdown": {"schema": {"type": "string"}}
            }
          },
          "400": {"description": "The PDF or URL was rejected", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}},
          "401": {"description": "Missing or wrong API key", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}},
          "413": {"description": "The PDF is larger than 32 MB", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}},
          "503": {"description": "PDFEXTRACT_API_KEY is not configured", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}}
        }
      }
    }
  }
}
"##;
