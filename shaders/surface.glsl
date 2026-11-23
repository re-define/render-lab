#include "cluster.glsl"
#ifdef GI_CLUSTER_HIT
#define OBJECT_BINDING 13
#define ADDRESS_BINDING 14
#define CAMERA_BINDING 15
#else
#include "visibility_image.glsl"
#define OBJECT_BINDING 1
#define ADDRESS_BINDING 3
#define CAMERA_BINDING 4
#endif
#include "plant_geometry.glsl"
#include "cluster_coverage.glsl"
struct Item { Instance obj; uvec4 refs; };
layout(set=0,binding=OBJECT_BINDING,std430) readonly buffer Objects { Item items[]; };
layout(set=0,binding=ADDRESS_BINDING,std430) readonly buffer Addresses { uvec2 addr[]; };
layout(set=0,binding=CAMERA_BINDING,std430) readonly buffer Camera { mat4 vp; vec4 eye; uvec2 screen; };

struct Vertex { vec4 p; vec4 n; };
layout(buffer_reference,std430,buffer_reference_align=16) readonly buffer Verts { Vertex v[]; };
layout(buffer_reference,std430,buffer_reference_align=4) readonly buffer Words { uint v[]; };
layout(buffer_reference,std430,buffer_reference_align=16) readonly buffer Meshlets { Cluster v[]; };

vec3 barycentric(mat3x4 clip,vec2 ndc) {
    vec3 x,y;
    for(int j=0;j<3;j++){x[j]=clip[j].x-ndc.x*clip[j].w;y[j]=clip[j].y-ndc.y*clip[j].w;}
    vec3 b=cross(x,y);return b/dot(b,vec3(1));
}

void surfaceTriangleWithPalette(Item item, uint primitive, uvec2 pixel, uvec2 palette,
                                out mat3x4 clip, out mat3 pos, out mat3 norms,
                                out mat3x2 uv, out uint mask) {
    Instance obj=item.obj;
    Words index=Words(addr[item.refs.y]);
    uvec3 tri;
    mask=7;
    bool deform = (obj.tex.w & WIND_DEFORM) != 0u;
    mat3 nm=transpose(inverse(mat3(obj.model)));
    if(item.refs.w!=0){
        Cluster c=Meshlets(addr[item.refs.z]).v[primitive>>6];
        if ((c.meta.w & VOXEL_CLUSTER) != 0u) {
            uint hit=primitive & 63u;
            uint faceWord=index.v[c.span.z+hit/2u];
            uint face=faceWord&7u;
            BrickWords words=BrickWords(addr[item.refs.x]);
            PlantCell cell=loadCell(words,c.span.x,faceWord>>3u);
            PlantBrick brick=loadBrick(words,c.span.x,(cell.location>>6u)&127u);
            vec4 bounds=cellBounds(brick,cell.location);
            uvec3 corners=voxelIndices(face,hit);
            mat4 pose=deform ? PlantPalette(palette).data[obj.routing.w+brick.joint] : mat4(1.);
            uint sampleBits=hashPixel(pixel, (primitive & ~63u) ^ (faceWord>>3u) ^ obj.routing.y);
            vec3 normal=normalize(nm * mat3(pose) * cellNormal(cell,face,sampleBits));
            for(uint j=0u;j<3u;++j) {
                pos[j] = (obj.model * pose * vec4(voxelCorner(bounds,corners[j]),1.0)).xyz;
                clip[j] = vp * vec4(pos[j],1.0);
                norms[j] = normal;
                uv[j] = cell.uv;
            }
            return;
        }
        uvec3 local=(uvec3(index.v[c.span.z+(primitive&63u)])>>uvec3(0,8,16))&255u;
        for(int j=0;j<3;j++)tri[j]=index.v[c.span.x+local[j]];
    }else{
        for(int j=0;j<3;j++)tri[j]=index.v[4*primitive+j];
        mask=index.v[4*primitive+3];
    }
    for(int j=0;j<3;j++){
        if ((obj.tex.w & PLANT) != 0u) {
            PlantVertex v = PlantVertices(addr[item.refs.x]).data[tri[j]];
            mat4 pose = deform ? plantMatrix(v,PlantPalette(palette),obj.routing.w) : mat4(1.0);
            vec4 p = obj.model * pose * vec4(v.p.xyz,1.0);
            clip[j]=vp*p;pos[j]=p.xyz;
            norms[j]=normalize(nm * mat3(pose) * v.n.xyz);
            uv[j]=vec2(v.p.w,v.n.w);
            continue;
        }
        Vertex v=Verts(addr[item.refs.x]).v[tri[j]];vec4 p=obj.model*vec4(v.p.xyz,1);
        clip[j]=vp*p;pos[j]=p.xyz;vec3 n=nm*v.n.xyz;norms[j]=dot(n,n)>0?normalize(n):vec3(0);uv[j]=vec2(v.p.w,v.n.w);
    }
}

void surfaceTriangle(Item item, uint primitive, uvec2 pixel, out mat3x4 clip, out mat3 pos,
                     out mat3 norms, out mat3x2 uv, out uint mask) {
    uvec2 palette = (item.obj.tex.w & WIND_DEFORM) != 0u ? addr[item.obj.routing.z] : uvec2(0u);
    surfaceTriangleWithPalette(item, primitive, pixel, palette, clip, pos, norms, uv, mask);
}
