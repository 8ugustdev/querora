#!/usr/bin/env python3
"""Generate the Querora source icon (1024x1024 PNG) for `tauri icon`.

Pure stdlib: draws an indigo→violet rounded square with a white "Q" ring
and tail. Output: scripts/icon-src.png
"""
import struct
import zlib

SIZE = 1024
CENTER = (SIZE * 0.46, SIZE * 0.44)
RING_R = SIZE * 0.27
RING_W = SIZE * 0.085
TAIL = ((SIZE * 0.60, SIZE * 0.58), (SIZE * 0.82, SIZE * 0.80))
TAIL_W = SIZE * 0.085
CORNER = SIZE * 0.18

C1 = (0x63, 0x66, 0xF1)  # indigo-500
C2 = (0x8B, 0x5C, 0xF6)  # violet-500


def clamp(v: float) -> int:
    return max(0, min(255, int(v)))


def bg(px: int, py: int) -> tuple[int, int, int]:
    t = (px + py) / (2 * SIZE)
    return (clamp(C1[0] + (C2[0] - C1[0]) * t),
            clamp(C1[1] + (C2[1] - C1[1]) * t),
            clamp(C1[2] + (C2[2] - C1[2]) * t))


def in_rounded(px: int, py: int) -> bool:
    if px >= CORNER and px < SIZE - CORNER:
        return True
    if py >= CORNER and py < SIZE - CORNER:
        return True
    for cx, cy in ((CORNER, CORNER), (SIZE - CORNER, CORNER),
                   (CORNER, SIZE - CORNER), (SIZE - CORNER, SIZE - CORNER)):
        if px < CORNER or px > SIZE - CORNER:
            if py < CORNER or py > SIZE - CORNER:
                if (px - cx) ** 2 + (py - cy) ** 2 <= CORNER ** 2:
                    return True
    return False


def dist_to_segment(p, a, b) -> float:
    ax, ay = a
    bx, by = b
    abx, aby = bx - ax, by - ay
    apx, apy = p[0] - ax, p[1] - ay
    t = max(0.0, min(1.0, (apx * abx + apy * aby) / (abx * abx + aby * aby)))
    qx, qy = ax + t * abx, ay + t * aby
    return ((p[0] - qx) ** 2 + (p[1] - qy) ** 2) ** 0.5


def main() -> None:
    rows = []
    for y in range(SIZE):
        row = bytearray([0])  # filter byte
        for x in range(SIZE):
            alpha = 255 if in_rounded(x, y) else 0
            color = bg(x, y)
            d = ((x - CENTER[0]) ** 2 + (y - CENTER[1]) ** 2) ** 0.5
            on_ring = abs(d - RING_R) <= RING_W / 2
            on_tail = dist_to_segment((x, y), TAIL[0], TAIL[1]) <= TAIL_W / 2
            # tail starts at ring edge, not inside it
            on_tail = on_tail and d > RING_R - TAIL_W
            if alpha and (on_ring or on_tail):
                color = (0xFF, 0xFF, 0xFF)
            row += bytes([color[0], color[1], color[2], alpha])
        rows.append(bytes(row))

    raw = b"".join(rows)

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(raw, 9))
           + chunk(b"IEND", b""))
    with open("scripts/icon-src.png", "wb") as f:
        f.write(png)
    print(f"wrote scripts/icon-src.png ({len(png)} bytes)")


if __name__ == "__main__":
    main()
