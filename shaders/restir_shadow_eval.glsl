void evaluateShadow(uvec2 xy) {
    uint i=indexAt(xy), seed=hash(i+hash(frame.x+70759u));
    Surface s=surfaces[i];
    vec3 color=vec3(0);
    if(s.id!=0u && options.y!=0u){
        if(options.x==0u){
            uint light=sampleLight(seed);
            color=visibleLight(s,light,seed,1u,0.012)*float(options.y);
        }else if(options.x==3u){
            seed=hash(i+20947u);
            uint count=min(options.y,options.w);
            for(uint j=0u;j<count;++j){
                uint lo=j*options.y/count, hi=(j+1u)*options.y/count;
                uint light=lo+min(uint(random(seed)*float(hi-lo)),hi-lo-1u);
                color+=visibleLight(s,light,seed,16u,0.012)*float(hi-lo);
            }
        }else{
            Reservoir r=selected[i];
            if(r.target>0.0) color=visibleLight(s,r.light,seed,1u,0.012)*normalization(r);
        }
    }
    if(s.id!=0u) color+=canopySky.rgb*skyVisibility(s);
    raw[i]=measure(color);
}
