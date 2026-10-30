"""Rust lexical delimiter checks used by the static source gate."""

def rust_delimiters(root, promptgen_root, generated, text, fail, display):
    opens={"(":")","[":"]","{":"}"}; closes={v:k for k,v in opens.items()}

    def char_literal_end(src: str, start: int):
        i=start+1
        if i>=len(src): return None
        if src[i]=="\\":
            i+=2
            if i<=len(src) and src[i-1]=="u" and i<len(src) and src[i]=="{":
                closing=src.find("}", i+1)
                if closing<0: return None
                i=closing+1
        else:
            if src[i] in "\n\r'": return None
            i+=1
        return i+1 if i<len(src) and src[i]=="'" else None

    def raw_string_start(src: str, start: int):
        i=start
        if src.startswith("br", i) or src.startswith("cr", i): i+=1
        if i>=len(src) or src[i]!="r": return None
        i+=1; hashes=0
        while i<len(src) and src[i]=="#": hashes+=1; i+=1
        if i>=len(src) or src[i]!='"': return None
        return i+1, hashes

    rust_files = list(root.rglob('*.rs')) + list(promptgen_root.rglob('*.rs'))
    for p in rust_files:
        if generated(p): continue
        src=text(p); stack=[]; block=0; i=0
        while i<len(src):
            if block:
                if src.startswith('/*',i): block+=1; i+=2
                elif src.startswith('*/',i): block-=1; i+=2
                else: i+=1
                continue
            if src.startswith('//',i):
                j=src.find('\n',i+2); i=len(src) if j<0 else j+1; continue
            if src.startswith('/*',i): block=1; i+=2; continue
            raw=raw_string_start(src,i)
            if raw is not None:
                content_start, hashes=raw; terminator='"'+('#'*hashes)
                j=src.find(terminator,content_start)
                if j<0: fail(f'{display(p)} unterminated raw string'); break
                i=j+len(terminator); continue
            if src[i]=='"' or (src[i] in {'b','c'} and i+1<len(src) and src[i+1]=='"'):
                if src[i]!='"': i+=1
                i+=1
                while i<len(src):
                    if src[i]=='\\': i+=2
                    elif src[i]=='"': i+=1; break
                    else: i+=1
                else: fail(f'{display(p)} unterminated string')
                continue
            if src[i]=="'":
                j=char_literal_end(src,i)
                if j is not None: i=j; continue
            ch=src[i]
            if ch in opens: stack.append((ch,i))
            elif ch in closes:
                if not stack or stack[-1][0]!=closes[ch]:
                    fail(f'{display(p)} delimiter mismatch at {i}'); break
                stack.pop()
            i+=1
        else:
            if block: fail(f'{display(p)} unterminated block comment')
            if stack: fail(f'{display(p)} unclosed delimiter {stack[-1][0]} at {stack[-1][1]}')
