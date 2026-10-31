struct Params { dims:vec4<u32> }
@group(0) @binding(0) var<storage,read> source:array<f32>;
@group(0) @binding(1) var<storage,read_write> dest:array<f32>;
@group(0) @binding(2) var<uniform> p:Params;
@compute @workgroup_size(16,16)
fn reduce(@builtin(global_invocation_id) id:vec3<u32>){
    if id.x>=p.dims.z || id.y>=p.dims.w{return;}
    for(var c=0u;c<3u;c++){
        var sum=0.0;
        for(var dy=0u;dy<2u;dy++){for(var dx=0u;dx<2u;dx++){
            let xy=min(id.xy*2u+vec2(dx,dy),p.dims.xy-1u);
            sum+=source[(xy.y*p.dims.x+xy.x)*3u+c]*0.25;
        }}
        dest[(id.y*p.dims.z+id.x)*3u+c]=sum;
    }
}
