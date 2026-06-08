import sys, re
def render(data, cols=140, rows=40):
    buf = [[' '] * cols for _ in range(rows)]
    text = data.decode('utf-8', errors='replace')
    r, c = 0, 0; i = 0
    while i < len(text):
        ch = text[i]
        if ch == '\x1b' and i + 1 < len(text):
            if text[i+1] == '[':
                m = re.match(r'\x1b\[([0-9;?]*)([a-zA-Z])', text[i:])
                if not m: i += 1; continue
                params, cmd = m.group(1), m.group(2)
                if cmd in ('H','f'):
                    if ';' in params:
                        rs, cs = params.split(';')
                        r = max(0, int(rs)-1) if rs else 0
                        c = max(0, int(cs)-1) if cs else 0
                    elif params: r = max(0, int(params)-1); c = 0
                    else: r, c = 0, 0
                elif cmd == 'A': r = max(0, r-(int(params) if params else 1))
                elif cmd == 'B': r = min(rows-1, r+(int(params) if params else 1))
                elif cmd == 'C': c = min(cols-1, c+(int(params) if params else 1))
                elif cmd == 'D': c = max(0, c-(int(params) if params else 1))
                elif cmd == 'J':
                    n = int(params) if params else 0
                    if n == 2: buf = [[' ']*cols for _ in range(rows)]; r, c = 0, 0
                elif cmd == 'K':
                    n = int(params) if params else 0
                    if n == 0:
                        for x in range(c, cols): buf[r][x] = ' '
                    elif n == 2:
                        for x in range(cols): buf[r][x] = ' '
                i += m.end(); continue
            elif text[i+1] == ']':
                m = re.match(r'\x1b\][0-9]*;[^\x07\x1b]*[\x07\x1b]', text[i:])
                if m: i += m.end(); continue
                i += 2; continue
            else: i += 2; continue
        elif ch == '\r': c = 0; i += 1
        elif ch == '\n': r = min(rows-1, r+1); i += 1
        elif ch == '\b': c = max(0, c-1); i += 1
        else:
            if 0 <= r < rows and 0 <= c < cols: buf[r][c] = ch
            c += 1
            if c >= cols: c = 0; r = min(rows-1, r+1)
            i += 1
    return [''.join(row).rstrip() for row in buf]
if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("input"); ap.add_argument("--cols", type=int, default=140); ap.add_argument("--rows", type=int, default=40)
    args = ap.parse_args()
    for l in render(open(args.input,'rb').read(), args.cols, args.rows): print(l)
