"""SRS movement search used to check that a placement can be reached.

Only 90-degree rotations with the standard SRS kick tables are modelled.
TETR.IO also allows 180-degree rotation, so "reachable here" implies
"reachable in TETR.IO", but not the other way round. y increases upward.
"""
from collections import deque
SHAPES={'T':[(1,2),(0,1),(1,1),(2,1)],'J':[(0,2),(0,1),(1,1),(2,1)],'L':[(2,2),(0,1),(1,1),(2,1)],
'S':[(1,2),(2,2),(0,1),(1,1)],'Z':[(0,2),(1,2),(1,1),(2,1)],'I':[(0,2),(1,2),(2,2),(3,2)],'O':[(1,2),(2,2),(1,1),(2,1)]}
def box(p): return 4 if p in 'IO' else 3
def rot_states(p):
    n=box(p); st=[SHAPES[p]]
    for _ in range(3): st.append([(y,n-1-x) for x,y in st[-1]])
    return st
JLSTZ={(0,1):[(0,0),(-1,0),(-1,1),(0,-2),(-1,-2)],(1,0):[(0,0),(1,0),(1,-1),(0,2),(1,2)],
(1,2):[(0,0),(1,0),(1,-1),(0,2),(1,2)],(2,1):[(0,0),(-1,0),(-1,1),(0,-2),(-1,-2)],
(2,3):[(0,0),(1,0),(1,1),(0,-2),(1,-2)],(3,2):[(0,0),(-1,0),(-1,-1),(0,2),(-1,2)],
(3,0):[(0,0),(-1,0),(-1,-1),(0,2),(-1,2)],(0,3):[(0,0),(1,0),(1,1),(0,-2),(1,-2)]}
IK={(0,1):[(0,0),(-2,0),(1,0),(-2,-1),(1,2)],(1,0):[(0,0),(2,0),(-1,0),(2,1),(-1,-2)],
(1,2):[(0,0),(-1,0),(2,0),(-1,2),(2,-1)],(2,1):[(0,0),(1,0),(-2,0),(1,-2),(-2,1)],
(2,3):[(0,0),(2,0),(-1,0),(2,1),(-1,-2)],(3,2):[(0,0),(-2,0),(1,0),(-2,-1),(1,2)],
(3,0):[(0,0),(1,0),(-2,0),(1,-2),(-2,1)],(0,3):[(0,0),(-1,0),(2,0),(-1,2),(2,-1)]}
TOP=24
def reachable(p,board,target):
    """True if piece p can come to rest exactly on target cells from spawn."""
    st=rot_states(p); kicks=IK if p=='I' else JLSTZ
    def cells(s,x,y): return [(x+a,y+b) for a,b in st[s]]
    def free(s,x,y): return all(0<=cx<10 and cy>=0 and (cx,cy) not in board for cx,cy in cells(s,x,y))
    start=(0,3,TOP-3)
    if not free(*start): return False
    seen={start}; q=deque([start]); target=frozenset(target)
    while q:
        s,x,y=q.popleft()
        if not free(s,x,y-1) and frozenset(cells(s,x,y))==target: return True
        nxt=[(s,x-1,y),(s,x+1,y),(s,x,y-1)]
        if p!='O':
            for ns in ((s+1)%4,(s+3)%4):
                for dx,dy in kicks[(s,ns)]:
                    if free(ns,x+dx,y+dy): nxt.append((ns,x+dx,y+dy)); break
        for n in nxt:
            if n not in seen and free(*n): seen.add(n); q.append(n)
    return False
