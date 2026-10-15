struct Dirty { origin:vec2<u32>, extent:vec2<u32> }
@group(0) @binding(0) var previous:texture_2d<f32>;
@group(0) @binding(1) var next:texture_storage_2d<rgba8unorm,write>;
@group(0) @binding(2) var<uniform> dirty:Dirty;
@compute @workgroup_size(8,8)
fn reduce(@builtin(global_invocation_id) id:vec3<u32>){
    if(any(id.xy>=dirty.extent)){return;}
    let pos=dirty.origin+id.xy;let dims=textureDimensions(next);
    if(any(pos>=dims)){return;}
    let pd=textureDimensions(previous);
    let ratio=vec2<f32>(pd)/vec2<f32>(dims);
    let begin=vec2<f32>(pos)*ratio;let end=vec2<f32>(pos+vec2<u32>(1u))*ratio;
    let start=vec2<u32>(floor(begin));
    var total=vec4<f32>(0.0);var count=0.0;
    for(var y=0u;y<3u;y++){for(var x=0u;x<3u;x++){
        let at=start+vec2<u32>(x,y);if(all(at<pd)){
            let overlap=max(vec2<f32>(0.0),min(vec2<f32>(at+vec2<u32>(1u)),end)-max(vec2<f32>(at),begin));
            let weight=overlap.x*overlap.y;
            total+=textureLoad(previous,vec2<i32>(at),0)*weight;count+=weight;
        }
    }}
    textureStore(next,vec2<i32>(pos),total/max(count,1.0));
}
