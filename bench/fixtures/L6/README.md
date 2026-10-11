# shop

Tiny shop CLI. Usage: `python3 app.py <cmd>` (state persists in shop_state.json).

Commands:

- `catalog` — list items
- `add SKU [--qty N]` — add to cart (lines for the same sku merge)
- `cart` — print cart JSON
- `checkout --coupon KIND:VAL` — KIND is `percent` or `flat`; may repeat, applied in order (each acts on the running total)

## TODO: wishlist (not implemented yet)

- `python3 app.py wishlist add SKU` — add item to wishlist (no-op if already there)
- `python3 app.py wishlist list` — print wishlist JSON
- `python3 app.py wishlist move SKU` — remove from wishlist, add to cart (respects cart merging); unknown sku exits non-zero
- wishlist persists in the same state file under `"wishlist"`

## Known issues

Cart merging and coupon compounding are buggy — `python3 test_shop.py` fails.
