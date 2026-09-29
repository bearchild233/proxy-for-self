"""Bounded private-worker request decoding; derived from excel-codex-bridge."""
import io
import json
import zlib
from starlette.requests import Request

class BodyTooLarge(ValueError):
    pass

def decode_json(raw: bytes, encoding: str, limit: int) -> dict:
    """Bound decompressed data too, before parsing an authenticated request."""
    if len(raw) > limit:
        raise BodyTooLarge
    encoding = encoding.strip().lower()
    if not encoding:
        encoding = "gzip" if raw.startswith(b"\x1f\x8b") else (
            "zstd" if raw.startswith(b"\x28\xb5\x2f\xfd") else "identity"
        )
    if encoding in {"gzip", "deflate"}:
        decoder = zlib.decompressobj(31 if encoding == "gzip" else zlib.MAX_WBITS)
        raw = decoder.decompress(raw, limit + 1)
        if len(raw) > limit or decoder.unconsumed_tail:
            raise BodyTooLarge
        if not decoder.eof or decoder.unused_data:
            raise ValueError("Invalid compressed body")
    elif encoding == "zstd":
        import zstandard
        try:
            with zstandard.ZstdDecompressor().stream_reader(io.BytesIO(raw)) as reader:
                raw = reader.read(limit + 1)
        except zstandard.ZstdError:
            raise ValueError("Invalid compressed body") from None
    elif encoding != "identity":
        raise ValueError("Unsupported content encoding")
    if len(raw) > limit:
        raise BodyTooLarge
    payload = json.loads(raw)
    if not isinstance(payload, dict):
        raise ValueError("Expected a JSON object")
    return payload

async def read_json(request: Request, limit: int) -> dict:
    raw = bytearray()
    async for chunk in request.stream():
        if len(raw) + len(chunk) > limit:
            raise BodyTooLarge
        raw.extend(chunk)
    return decode_json(bytes(raw), request.headers.get("content-encoding", ""), limit)
