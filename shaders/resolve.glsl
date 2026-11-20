#include "cluster.glsl"
layout(local_size_x=64) in;
layout(set=0,binding=0,std430) readonly buffer Visibility {uvec2 ids[];};
struct Item {Instance obj;uvec4 refs;};
layout(set=0,binding=1,std430) readonly buffer Objects {Item items[];};
layout(set=0,binding=2,std430) readonly buffer Work {uint work[];};
layout(set=0,binding=3,std430) readonly buffer Addresses {uvec2 addr[];};
layout(set=0,binding=4,std430) readonly buffer Camera {mat4 vp;vec4 eye;uvec2 screen;};
layout(set=0,binding=5,std430) readonly buffer Atlas {uint texels[];};
layout(set=0,binding=6,std430) buffer Output {uint pixels[];};
layout(set=0,binding=7,std430) readonly buffer Bins{uvec4 bins[];};
layout(push_constant) uniform Params{layout(offset=32)uint materialBin;}p;
struct Vertex {vec4 p;vec4 n;};
layout(buffer_reference,std430,buffer_reference_align=16) readonly buffer Verts {Vertex v[];};
layout(buffer_reference,std430,buffer_reference_align=4) readonly buffer Words {uint v[];};
layout(buffer_reference,std430,buffer_reference_align=16) readonly buffer Meshlets {Cluster v[];};
const vec3 palette[10]=vec3[](vec3(57,212,155),vec3(78,165,255),vec3(255,206,75),vec3(250,118,69),vec3(204,111,246),vec3(32,207,217),vec3(244,110,165),vec3(138,210,74),vec3(170,145,250),vec3(249,238,213));
vec3 barycentric(mat3x4 clip,vec2 ndc){
    vec3 x,y;
    for(int j=0;j<3;j++){x[j]=clip[j].x-ndc.x*clip[j].w;y[j]=clip[j].y-ndc.y*clip[j].w;}
    vec3 b=cross(x,y);return b/dot(b,vec3(1));
}
uint bgra(vec3 rgb){return packUnorm4x8(vec4(rgb.bgr,1));}
#ifdef NEURAL_BASE_COLOR
#include "materials/texture_grid.glsl"
#elif defined(GAUSSIAN_BASE_COLOR)
#include "materials/gaussian.glsl"
#endif
#ifdef SURFACE_MATERIAL
vec3 shadeSurface(vec3 albedo, vec3 normal, vec2 uv, Instance obj);
#endif
void main(){
    uint idx=(gl_WorkGroupID.x+32768u*gl_WorkGroupID.y)*64+gl_LocalInvocationID.x;
    uvec4 bin=bins[p.materialBin];if(idx>=bin.x)return;
    uint pixel=work[bin.y+idx];uvec2 vis=ids[pixel];
    Item item=items[vis.x-1];Instance obj=item.obj;
    Words index=Words(addr[item.refs.y]);uvec3 tri;uint mask=7;
    if(item.refs.w!=0){
        Cluster c=Meshlets(addr[item.refs.z]).v[vis.y>>6];
        if((obj.tex.w&32u)!=0u){pixels[pixel]=bgra(palette[min(c.meta.z,9u)]/255.);return;}
        uvec3 local=(uvec3(index.v[c.span.z+(vis.y&63u)])>>uvec3(0,8,16))&255u;
        for(int j=0;j<3;j++)tri[j]=index.v[c.span.x+local[j]];
    }else{
        for(int j=0;j<3;j++)tri[j]=index.v[4*vis.y+j];mask=index.v[4*vis.y+3];
    }
    mat3x4 clip;mat3 pos,norms;mat3x2 uv;mat3 nm=transpose(inverse(mat3(obj.model)));
    for(int j=0;j<3;j++){
        Vertex v=Verts(addr[item.refs.x]).v[tri[j]];vec4 p=obj.model*vec4(v.p.xyz,1);
        clip[j]=vp*p;pos[j]=p.xyz;vec3 n=nm*v.n.xyz;norms[j]=dot(n,n)>0?normalize(n):vec3(0);uv[j]=vec2(v.p.w,v.n.w);
    }
    vec2 xy=vec2(pixel%screen.x,pixel/screen.x)+.5,stepNdc=2./vec2(screen);
    vec2 ndc=xy*stepNdc-1.;vec3 b=barycentric(clip,ndc);
    vec3 n=norms*b;if(dot(n,n)<1e-12)n=cross(pos[1]-pos[0],pos[2]-pos[0]);
    vec3 N=normalize(n),V=normalize(eye.xyz-pos*b),L=normalize(vec3(-.45,.8,.65));
    if(dot(N,V)<0.)N=-N;
    uvec2 tex=min(uvec2(fract(uv*b)*vec2(obj.tex.yz)),obj.tex.yz-1u);
#ifdef NEURAL_BASE_COLOR
    vec3 encodedColor=decodeTextureGrid(obj.tex.x,tex);
#elif defined(GAUSSIAN_BASE_COLOR)
    vec3 encodedColor=gaussianColor(obj.tex.x,uv*b);
#else
    vec3 encodedColor=unpackUnorm4x8(texels[obj.tex.x+tex.y*obj.tex.y+tex.x]).rgb;
#endif
    vec3 albedo=obj.color.rgb*pow(encodedColor,vec3(2.2));
#ifdef SURFACE_MATERIAL
    vec3 light=shadeSurface(albedo,N,uv*b,obj);
#else
    vec3 light=shadeMaterial(albedo,N,V,L,obj.params.xy);
#endif
    if((obj.tex.w&1u)!=0u){
        vec3 w=vec3(clip[0].w,clip[1].w,clip[2].w);
        vec3 a=b*w/dot(b,w),bx=barycentric(clip,ndc+vec2(stepNdc.x,0)),by=barycentric(clip,ndc+vec2(0,stepNdc.y));
        vec3 fw=abs(bx*w/dot(bx,w)-a)+abs(by*w/dot(by,w)-a);
        vec3 dist=a/max(fw,vec3(.00001));for(uint j=0;j<3;j++)if((mask&(1u<<j))==0)dist[j]=1e5;
        light*=mix(.16,1.,smoothstep(.55,1.5,min(dist.x,min(dist.y,dist.z))));
    }
    pixels[pixel]=bgra(pow(max(light,0.),vec3(1./2.2)));
}
