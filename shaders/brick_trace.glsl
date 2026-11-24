// Pixel-center intersection shared by software raster, mesh, and fragment shaders.
int minAxis(vec3 v) { return v.x<=v.y && v.x<=v.z ? 0 : (v.y<=v.z?1:2); }
bool hitBrick(vec3 origin,vec3 rayDirection,uvec4 masks,uint bounds,uint firstCell,uint page,vec4 depthRow,vec4 wRow,
              out float depth,out uint primitive) {
    vec3 dir=normalize(rayDirection);
    // Clip the ray to the occupied cells, including for single-cell bricks.
    ivec3 lo=ivec3(brickGrid(bounds)), hi=ivec3(brickGrid(bounds>>6u))+1;
    vec3 inv;
    for(int a=0;a<3;a++) {
        if(abs(dir[a])<1e-12 && (origin[a]<float(lo[a]) || origin[a]>=float(hi[a]))) return false;
        inv[a]=(dir[a]<0.?-1.:1.)/max(abs(dir[a]),1e-12);
    }
    vec3 first=(vec3(lo)-origin)*inv, last=(vec3(hi)-origin)*inv;
    vec3 enter=min(first,last), leave=max(first,last);
    for(int a=0;a<3;a++) if(abs(dir[a])<1e-12) { enter[a]=-1e30; leave[a]=1e30; }
    int axis=minAxis(-enter);
    float t=enter[axis], end=min(leave.x,min(leave.y,leave.z));
    if(end<=max(t,0.)) return false;
    bool hasEntry=t>=0.;
    t=max(t,0.);
    ivec3 stepDir=ivec3(sign(dir));
    ivec3 cell=ivec3(clamp(floor(origin+dir*t+vec3(stepDir)*0.00001),vec3(lo),vec3(hi-1)));
    vec3 next=(vec3(cell)+vec3(greaterThan(stepDir,ivec3(0)))-origin)*inv;
    for(int a=0;a<3;a++) if(abs(dir[a])<1e-12) next[a]=1e30;
    uint tag=0u;
    int face=-1;
    // A ray can cross at most nine internal grid planes in a 4x4x4 brick.
    for(int visit=0;visit<12;visit++) {
        uint bit=uint(cell.x+4*cell.y+16*cell.z), halfIndex=bit>>5u;
        uint flag=1u<<(bit&31u), word=masks[halfIndex];
        int exitAxis=minAxis(next);
        float exitT=next[exitAxis];
        if((word & flag)!=0u && exitT>t) {
            uint rank=bitCount(word & (flag-1u))+(halfIndex!=0u?uint(bitCount(masks.x)):0u);
            tag=BrickWords(masks.zw).word[firstCell+6u*rank+5u];
            uint exposed=(tag>>13u)&63u;
            int entryFace=2*axis+int(stepDir[axis]<0);
            int exitFace=2*exitAxis+int(stepDir[exitAxis]>0);
            if(hasEntry && (exposed&(1u<<entryFace))!=0u) { face=entryFace; break; }
            if((exposed&(1u<<exitFace))!=0u) { face=exitFace; t=exitT; break; }
        }
        if(exitT>=end) break;
        cell[exitAxis]+=stepDir[exitAxis];
        if(cell[exitAxis]<lo[exitAxis] || cell[exitAxis]>=hi[exitAxis]) break;
        next[exitAxis]+=abs(inv[exitAxis]);
        axis=exitAxis; t=exitT; hasEntry=true;
    }
    if(face<0) return false;
    vec4 hit=vec4(origin+dir*t,1.);
    depth=dot(depthRow,hit)/dot(wRow,hit);
    if(!(depth>=0. && depth<=1.)) return false;
    uint quad=((tag>>19u)&31u)+bitCount((tag>>13u)&((1u<<face)-1u));
    // Use the same face winding and diagonal as the triangle CLAS and resolve.
    const int uAxis[6]=int[](2,1,0,2,1,0),vAxis[6]=int[](1,2,2,0,0,1);
    vec3 within=hit.xyz-vec3(cell);
    uint triangle=2u*quad+uint(within[vAxis[face]]>within[uAxis[face]]);
    primitive=((page+(tag>>24u))<<6u)|triangle;
    return true;
}
