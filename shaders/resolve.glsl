#include "surface.glsl"
layout(local_size_x=64) in;
layout(set=0,binding=2,std430) readonly buffer Work {uint work[];};
layout(set=0,binding=5,std430) readonly buffer Atlas {uint texels[];};
#ifdef HDR_OUTPUT
layout(set=0,binding=6,std430) writeonly buffer Output {vec4 pixels[];};
#else
layout(set=0,binding=6,std430) writeonly buffer Output {uint pixels[];};
#endif
layout(set=0,binding=7,std430) readonly buffer Bins{uvec4 bins[];};
#ifndef RESOLVE_BINDING_BYTES
#define RESOLVE_BINDING_BYTES 32
#endif
layout(push_constant) uniform Params{layout(offset=RESOLVE_BINDING_BYTES)uint materialBin;}p;
const vec3 palette[10]=vec3[](vec3(57,212,155),vec3(78,165,255),vec3(255,206,75),vec3(250,118,69),vec3(204,111,246),vec3(32,207,217),vec3(244,110,165),vec3(138,210,74),vec3(170,145,250),vec3(249,238,213));
uint bgra(vec3 rgb){return packUnorm4x8(vec4(rgb.bgr,1));}
void storeColor(uint pixel,vec3 linear) {
#ifdef HDR_OUTPUT
    pixels[pixel]=vec4(max(linear,0.0),1.0);
#else
    pixels[pixel]=bgra(pow(max(linear,0.0),vec3(1./2.2)));
#endif
}
#ifdef NEURAL_BASE_COLOR
#include "materials/texture_grid.glsl"
#elif defined(GAUSSIAN_BASE_COLOR)
#include "materials/gaussian.glsl"
#endif
#ifdef SURFACE_MATERIAL
vec3 shadeSurface(vec3 albedo, vec3 normal, vec2 uv, Instance obj);
#endif
#ifdef PIXEL_POSITION_MATERIAL
vec3 shadePosition(uint pixel, vec3 albedo, vec3 position, vec3 normal, vec3 view, vec2 uv, Instance obj);
#endif
#ifdef POSITION_MATERIAL
vec3 shadePosition(vec3 albedo, vec3 position, vec3 normal, vec3 view, vec2 uv, Instance obj);
#endif
#ifdef LIGHTING_MATERIAL
vec3 shadeLighting(uint pixel, vec3 albedo, vec3 position, vec3 normal, vec3 view, Instance obj);
#endif
void main(){
    uint idx=(gl_WorkGroupID.x+32768u*gl_WorkGroupID.y)*64+gl_LocalInvocationID.x;
    uvec4 bin=bins[p.materialBin];if(idx>=bin.x)return;
    uint pixel=work[bin.y+idx];uvec2 vis=visibilityAt(pixel,screen);
    Item item=items[vis.x-1];Instance obj=item.obj;
    if ((obj.tex.w & GEOMETRY_VIEW) != 0u) {
        bool voxel = item.refs.w != 0u && (Meshlets(addr[item.refs.z]).v[vis.y>>6].meta.w & VOXEL_CLUSTER) != 0u;
        vec3 pathColor = voxel ? vec3(0.08,0.57,0.83) : vec3(0.83,0.36,0.075);
        storeColor(pixel,pathColor);
        return;
    }
    if(item.refs.w!=0 && (obj.tex.w&32u)!=0u){
        Cluster c=Meshlets(addr[item.refs.z]).v[vis.y>>6];
        vec3 color=palette[min(c.meta.z,9u)]/255.;
#ifdef HDR_OUTPUT
        storeColor(pixel,pow(color,vec3(2.2)));
#else
        pixels[pixel]=bgra(color);
#endif
        return;
    }
    mat3x4 clip;mat3 pos,norms;mat3x2 uv;uint mask;
    uvec2 pixelCoord=uvec2(pixel%screen.x,pixel/screen.x);
    surfaceTriangle(item,vis.y,pixelCoord,clip,pos,norms,uv,mask);
    vec2 xy=vec2(pixelCoord)+.5,stepNdc=2./vec2(screen);
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
#ifdef FILTERED_BASE_COLOR
    vec2 coord=clamp((uv*b)*vec2(obj.tex.yz)-.5,vec2(0),vec2(obj.tex.yz-1u));
    uvec2 lo=uvec2(coord),hi=min(lo+1,obj.tex.yz-1u);
    vec2 f=fract(coord);
    vec3 x0=mix(unpackUnorm4x8(texels[obj.tex.x+lo.y*obj.tex.y+lo.x]).rgb,unpackUnorm4x8(texels[obj.tex.x+lo.y*obj.tex.y+hi.x]).rgb,f.x);
    vec3 x1=mix(unpackUnorm4x8(texels[obj.tex.x+hi.y*obj.tex.y+lo.x]).rgb,unpackUnorm4x8(texels[obj.tex.x+hi.y*obj.tex.y+hi.x]).rgb,f.x);
    encodedColor=mix(x0,x1,f.y);
#endif
    vec3 albedo=obj.color.rgb*pow(encodedColor,vec3(2.2));
#ifdef PIXEL_POSITION_MATERIAL
    vec3 light=shadePosition(pixel,albedo,pos*b,N,V,uv*b,obj);
#elif defined(POSITION_MATERIAL)
    vec3 light=shadePosition(albedo,pos*b,N,V,uv*b,obj);
#elif defined(SURFACE_MATERIAL)
    vec3 light=shadeSurface(albedo,N,uv*b,obj);
#elif defined(LIGHTING_MATERIAL)
    vec3 light=shadeLighting(pixel,albedo,pos*b,N,V,obj);
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
    storeColor(pixel,light);
}
