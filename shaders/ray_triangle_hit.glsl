struct RayVertex { vec4 positionU; vec4 normalV; };
layout(buffer_reference,std430,buffer_reference_align=16) readonly buffer HitVertices { RayVertex vertices[]; };
layout(buffer_reference,std430,buffer_reference_align=16) readonly buffer HitFaces { uvec4 faces[]; };
layout(binding=5,std430) readonly buffer Geometry { uvec2 streams[]; };
layout(binding=6,std430) readonly buffer Materials { uint regularCount; uint slots[]; };
void triangleHit(uint instance, uint primitive, vec3 b, mat3 worldToObject, mat3 toWorld, inout SceneRayHit result) {
    uvec4 face=HitFaces(streams[2u*instance+1u]).faces[primitive];
    HitVertices source=HitVertices(streams[2u*instance]);
    mat3 normalToWorld=transpose(worldToObject);
    mat3 pos; vec3 n=vec3(0); vec2 uv=vec2(0);
    for(uint j=0;j<3u;++j) {
        RayVertex v=source.vertices[face[j]];
        pos[j]=v.positionU.xyz;
        vec3 transformed=normalToWorld*v.normalV.xyz;
        if(dot(transformed,transformed)>1e-16) n+=b[j]*normalize(transformed);
        uv+=b[j]*vec2(v.positionU.w,v.normalV.w);
    }
    if(dot(n,n)<=1e-10) {
        n=cross(toWorld*(pos[1]-pos[0]),toWorld*(pos[2]-pos[0]));
    }
    result.normal=normalize(n);
    result.uv=uv;
    result.material=slots[instance]==0xffffffffu ? face.w : slots[instance];
}
