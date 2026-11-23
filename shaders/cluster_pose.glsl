uvec2 clusterFade(Asset asset, Instance obj, Cluster cluster, View view) {
    float low=0., high=1.;
    if (foliageLod(obj)) {
        Group owner = asset.groups.data[cluster.meta.x];
        high = refinementWeight(owner.error, owner.sphere, obj, view);
        if (cluster.meta.y != NONE) {
            Group finer = asset.groups.data[cluster.meta.y];
            low = refinementWeight(finer.error, finer.sphere, obj, view);
        }
    }
    return uvec2(packUnorm2x16(vec2(low, high)), obj.routing.y);
}
vec4 clusterPosition(Asset asset, Instance obj,Cluster cluster,bool voxel,uint i) {
    uvec4 span=cluster.span;
    bool deform=(obj.tex.w&WIND_DEFORM)!=0u;
    if(voxel){
        BrickWords words=BrickWords(uvec2(asset.vertices));
        uint location=words.word[cellAddress(words,span.x,(cluster.meta.w>>1u)+i/8u)+5u];
        PlantBrick brick=loadBrick(words,span.x,(location>>6u)&127u);
        vec4 p=vec4(voxelCorner(cellBounds(brick,location),i&7u),1.);
        return deform ? jointPalette[obj.routing.w+brick.joint]*p : p;
    }
    uint v=asset.indices.data[span.x+i];
    if((obj.tex.w&PLANT)!=0u){
        PlantVertex vertex=PlantVertices(uvec2(asset.vertices)).data[v];
        vec4 p=vec4(vertex.p.xyz,1.);
        if(deform){
            mat4 pose=mat4(0.);
            for(uint k=0u;k<4u;k++) if(vertex.weights[k]!=0.) pose+=jointPalette[obj.routing.w+vertex.joints[k]]*vertex.weights[k];
            p=pose*p;
        }
        return p;
    }
    return vec4(asset.vertices.data[2u*v].xyz,1.);
}
uvec3 clusterIndices(Asset asset,Cluster cluster,bool voxel,uint t){
    uvec4 span=cluster.span;
    if(voxel)return voxelIndices(asset.indices.data[span.z+t/2u]-8u*(cluster.meta.w>>1u),t);
    return (uvec3(asset.indices.data[span.z+t])>>uvec3(0,8,16))&255u;
}
