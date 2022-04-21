use super::RenderState;
use crate::assets;
use crate::assets::Texture;
use crate::camera::Camera;
use crate::types::*;
use crate::vulkan::Vulkan;
use bytemuck::{Pod, Zeroable};
use std::collections::HashMap;
use std::sync::Arc;
use vulkano::buffer::{BufferUsage, CpuBufferPool, ImmutableBuffer};
use vulkano::command_buffer::AutoCommandBufferBuilder;
use vulkano::descriptor_set::single_layout_pool::SingleLayoutDescSet;
use vulkano::descriptor_set::PersistentDescriptorSet;
use vulkano::descriptor_set::SingleLayoutDescSetPool;
use vulkano::pipeline::graphics::input_assembly::InputAssemblyState;
use vulkano::pipeline::graphics::rasterization::RasterizationState;
use vulkano::pipeline::graphics::vertex_input::BuffersDefinition;
use vulkano::pipeline::graphics::viewport::ViewportState;
use vulkano::pipeline::GraphicsPipeline;
use vulkano::pipeline::Pipeline;
use vulkano::render_pass::Subpass;
use vulkano::sampler::Sampler;
use vulkano::{
    buffer::cpu_pool::CpuBufferPoolChunk, pipeline::graphics::color_blend::ColorBlendState,
};

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Default, Pod, Debug, PartialEq)]
pub struct SingleRenderState {
    uv_region: Rect,

    position_rot: Vec4,

    size_alpha: Vec3,
}
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Default, Pod, Debug, PartialEq)]
struct InstanceData {
    uv_region: [f32; 4],
    position_rot: [f32; 4],
    size_alpha: [f32; 3],
}
vulkano::impl_vertex!(InstanceData, uv_region, position_rot, size_alpha);

impl SingleRenderState {
    pub fn new(uv_region: Rect, position: Vec3, rot: f32, size: Vec2, alpha: f32) -> Self {
        Self {
            uv_region,
            position_rot: Vec4::new(position.x, position.y, position.z, rot),
            size_alpha: Vec3::new(size.x, size.y, alpha),
        }
    }
}
impl super::SingleRenderState for SingleRenderState {
    fn interpolate(&self, other: &Self, r: f32) -> Self {
        Self {
            position_rot: {
                let pos = self.position_rot.truncated().interpolate_limit(
                    &other.position_rot.truncated(),
                    r,
                    10.0,
                );
                Vec4::new(
                    pos.x,
                    pos.y,
                    pos.z,
                    self.position_rot
                        .w
                        .interpolate_limit(&other.position_rot.w, r, PI / 4.0),
                )
            },
            size_alpha: {
                let sz = self.size_alpha.truncated().interpolate_limit(
                    &other.size_alpha.truncated(),
                    r,
                    0.5,
                );
                Vec3::new(
                    sz.x,
                    sz.y,
                    self.size_alpha
                        .z
                        .interpolate_limit(&other.size_alpha.z, r, 0.25),
                )
            },
            uv_region: other.uv_region,
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendMode {
    Additive,
    //Alpha,
}
struct BatchData {
    material_pds: Arc<vulkano::descriptor_set::PersistentDescriptorSet>,
    instance_data: Vec<InstanceData>,
    instance_buf:
        Option<Arc<CpuBufferPoolChunk<InstanceData, Arc<vulkano::memory::pool::StdMemoryPool>>>>,
    index_buf: Arc<ImmutableBuffer<[u16]>>,
}

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Default, Pod)]
struct Uniforms {
    view: Mat4,
    proj: Mat4,
}

pub struct Renderer {
    pipeline: Arc<vulkano::pipeline::GraphicsPipeline>,
    sampler: Arc<Sampler>,
    // we'll use one uniform buffer across all batches.
    // it will be the projection-view transform.
    uniform_buffers: CpuBufferPool<Uniforms>,
    uniform_pds: SingleLayoutDescSetPool,
    uniform_binding: Option<Arc<SingleLayoutDescSet>>,
    index_buf: Arc<ImmutableBuffer<[u16]>>,
    instance_pool: CpuBufferPool<InstanceData, Arc<vulkano::memory::pool::StdMemoryPool>>,
    batches: HashMap<(assets::TextureRef, BlendMode), BatchData>,
}
impl super::Renderer for Renderer {
    type BatchRenderKey = (assets::TextureRef, BlendMode);
    type SingleRenderState = SingleRenderState;
}
impl Renderer {
    pub fn new(vulkan: &mut Vulkan) -> Self {
        mod vs {
            vulkano_shaders::shader! {
                ty: "vertex",
                src: "
#version 450

// vertex attributes---none!
// instance data
layout(location = 0) in vec4 uv_region;
layout(location = 1) in vec4 position_rot;
layout(location = 2) in vec3 size_alpha;

// outputs
layout(location = 0) out vec3 out_uv_a;

// uniforms
layout(set=0, binding=0) uniform BatchData { mat4 view; mat4 proj; };

void main() {
  float w = size_alpha.x;
  float h = size_alpha.y;
  float rot = position_rot.w;
  float alpha = size_alpha.z;

  // 0: TL, 1: BL, 2: BR, 3: TR
  vec2 posns[] = {
    vec2(-0.5, 0.5),
    vec2(-0.5, -0.5),
    vec2(0.5, -0.5),
    vec2(0.5, 0.5),
  };
  vec2 pos = posns[gl_VertexIndex].xy;
  vec2 uv_corner = vec2(uv_region.x,1.0-uv_region.y) + vec2(uv_region.z*(pos.x+0.5),uv_region.w*(1.0-(pos.y+0.5)));
  vec4 center = view * vec4(position_rot.xyz, 1.0);
  vec2 rot_pos = vec2(
    pos.x*w*cos(rot)-pos.y*h*sin(rot),
    pos.y*h*cos(rot)+pos.x*w*sin(rot)
  );
  gl_Position = proj * vec4(rot_pos.x+center.x,rot_pos.y+center.y,center.z,1.0);
  out_uv_a = vec3(uv_corner.xy, alpha);
}
"
            }
        }

        mod fs {
            vulkano_shaders::shader! {
                ty: "fragment",
                src: "
#version 450

layout(set = 1, binding = 0) uniform sampler2D tex;
layout(location = 0) in vec3 uv_a;
layout(location = 0) out vec4 f_color;

void main() {
  vec4 col = texture(tex, uv_a.xy);
  if (col.a < 0.01) { discard; }
  col.a *= uv_a.z;
  f_color = col;
}
"
            }
        }

        let vs = vs::load(vulkan.device.clone()).unwrap();
        let fs = fs::load(vulkan.device.clone()).unwrap();
        use vulkano::sampler::SamplerCreateInfo;
        let sampler = Sampler::new(vulkan.device.clone(), SamplerCreateInfo::default()).unwrap();
        use vulkano::pipeline::graphics::depth_stencil::*;
        let pipeline = GraphicsPipeline::start()
            .vertex_input_state(BuffersDefinition::new().instance::<InstanceData>())
            .vertex_shader(vs.entry_point("main").unwrap(), ())
            .input_assembly_state(InputAssemblyState::new().topology(
                vulkano::pipeline::graphics::input_assembly::PrimitiveTopology::TriangleList,
            ))
            .viewport_state(ViewportState::viewport_dynamic_scissor_irrelevant())
            .fragment_shader(fs.entry_point("main").unwrap(), ())
            .rasterization_state(
                RasterizationState::new()
                    .cull_mode(vulkano::pipeline::graphics::rasterization::CullMode::Back)
                    .front_face(
                        vulkano::pipeline::graphics::rasterization::FrontFace::CounterClockwise,
                    ),
            )
            .color_blend_state(ColorBlendState::new(1).blend_additive())
            .depth_stencil_state(DepthStencilState {
                depth: Some(DepthState {
                    compare_op: vulkano::pipeline::StateMode::Fixed(CompareOp::Greater),
                    enable_dynamic: false,
                    write_enable: vulkano::pipeline::StateMode::Fixed(false),
                }),
                depth_bounds: None,
                stencil: None,
            })
            .render_pass(Subpass::from(vulkan.render_pass.clone(), 0).unwrap())
            .build(vulkan.device.clone())
            .unwrap();

        let uniform_buffers = CpuBufferPool::uniform_buffer(vulkan.device.clone());
        let uniform_pds =
            SingleLayoutDescSetPool::new(pipeline.layout().set_layouts().get(0).unwrap().clone());
        let instance_pool = CpuBufferPool::vertex_buffer(vulkan.device.clone());

        let (index_buf, fut) = ImmutableBuffer::from_iter(
            [0_u16, 1, 2, 0, 2, 3].into_iter(),
            BufferUsage::index_buffer(),
            vulkan.queue.clone(),
        )
        .unwrap();
        vulkan.wait_for(Box::new(fut));

        Self {
            sampler,
            pipeline,
            uniform_buffers,
            uniform_pds,
            index_buf,
            instance_pool,
            batches: HashMap::new(),
            uniform_binding: None,
        }
    }
    pub fn push_models<'a>(
        &mut self,
        (tr, bm): (assets::TextureRef, BlendMode),
        texture: &Texture,
        dat: impl IntoIterator<Item = &'a SingleRenderState>,
    ) {
        use std::collections::hash_map::Entry;
        let insts = dat.into_iter().copied();
        match self.batches.entry((tr, bm)) {
            Entry::Vacant(v) => {
                let mut b = Self::create_batch(
                    self.pipeline.clone(),
                    self.sampler.clone(),
                    texture,
                    self.index_buf.clone(),
                );
                b.push_instances(insts);
                v.insert(b);
            }
            Entry::Occupied(v) => v.into_mut().push_instances(insts),
        }
    }
    fn create_batch(
        pipeline: Arc<vulkano::pipeline::GraphicsPipeline>,
        sampler: Arc<Sampler>,
        texture: &Texture,
        index_buf: Arc<ImmutableBuffer<[u16]>>,
    ) -> BatchData {
        BatchData {
            instance_data: vec![],
            instance_buf: None,
            index_buf,
            material_pds: PersistentDescriptorSet::new(
                pipeline.layout().set_layouts().get(1).unwrap().clone(),
                [
                    vulkano::descriptor_set::WriteDescriptorSet::image_view_sampler(
                        0,
                        vulkano::image::view::ImageView::new_default(texture.texture.clone())
                            .unwrap(),
                        sampler,
                    ),
                ],
            )
            .unwrap(),
        }
    }
    pub fn prepare(&mut self, rs: &RenderState, assets: &assets::Assets, camera: &Camera) {
        for ((tex_id, bm), v) in rs.billboards.interpolated.values() {
            let tex = assets.texture(*tex_id);
            self.push_models((*tex_id, *bm), tex, std::iter::once(v));
        }
        for ((tex_id, bm), vs) in rs.billboards.raw.iter() {
            let tex = assets.texture(*tex_id);
            self.push_models((*tex_id, *bm), tex, vs);
        }
        self.prepare_draw(camera);
    }
    fn prepare_draw(&mut self, camera: &Camera) {
        let buf = self
            .uniform_buffers
            .next(Uniforms {
                view: camera.transform.into_homogeneous_matrix(),
                proj: camera.projection.as_matrix(camera.ratio),
            })
            .unwrap();
        let uds = self
            .uniform_pds
            .next(vec![vulkano::descriptor_set::WriteDescriptorSet::buffer(
                0, buf,
            )])
            .unwrap();
        self.uniform_binding = Some(uds);
        for (_k, b) in self.batches.iter_mut() {
            b.prepare_draw(&self.instance_pool);
        }
    }
    pub fn draw<P, L>(&mut self, builder: &mut AutoCommandBufferBuilder<P, L>) {
        let uds = self.uniform_binding.clone().unwrap();

        builder.bind_pipeline_graphics(self.pipeline.clone());

        for (_b, dat) in self.batches.iter() {
            dat.draw(self.pipeline.clone(), uds.clone(), builder);
        }
        self.clear_frame();
    }
    fn clear_frame(&mut self) {
        // delete batch data for objects that didn't get rendered this frame.
        // TODO: something more sophisticated!
        self.batches.retain(|_k, v| !v.is_empty());
        // delete instance data from each batch, but don't throw away the vecs' allocations
        self.batches.iter_mut().for_each(|(_k, v)| v.clear_frame());
    }
}

impl BatchData {
    fn prepare_draw(
        &mut self,
        instance_pool: &CpuBufferPool<InstanceData, Arc<vulkano::memory::pool::StdMemoryPool>>,
    ) {
        self.instance_buf = Some(
            instance_pool
                .chunk(self.instance_data.iter().copied())
                .unwrap(),
        );
    }
    fn draw<P, L>(
        &self,
        pipeline: Arc<GraphicsPipeline>,
        unis: Arc<vulkano::descriptor_set::single_layout_pool::SingleLayoutDescSet>,
        builder: &mut AutoCommandBufferBuilder<P, L>,
    ) {
        builder
            .bind_vertex_buffers(0, [self.instance_buf.clone().unwrap()])
            .bind_index_buffer(self.index_buf.clone())
            .bind_descriptor_sets(
                vulkano::pipeline::PipelineBindPoint::Graphics,
                (*pipeline).layout().clone(),
                0,
                unis,
            )
            .bind_descriptor_sets(
                vulkano::pipeline::PipelineBindPoint::Graphics,
                (*pipeline).layout().clone(),
                1,
                self.material_pds.clone(),
            )
            .draw_indexed(6, self.instance_data.len() as u32, 0, 0, 0)
            .unwrap();
    }
    fn clear_frame(&mut self) {
        self.instance_data.clear();
    }
    fn is_empty(&self) -> bool {
        self.instance_data.is_empty()
    }
    fn push_instances(&mut self, insts: impl IntoIterator<Item = SingleRenderState>) {
        // Safety: srs and instancedata have the same layout, both are Pod
        self.instance_data.extend(
            insts
                .into_iter()
                .map(|srs| unsafe { std::mem::transmute::<_, InstanceData>(srs) }),
        );
    }
}
