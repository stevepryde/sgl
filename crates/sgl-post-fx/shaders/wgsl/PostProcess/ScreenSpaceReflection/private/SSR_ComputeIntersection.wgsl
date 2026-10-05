// WGSL port of DiligentFX Shaders/PostProcess/ScreenSpaceReflection/private/SSR_ComputeIntersection.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// DFX-18 adds the depth-tolerance pass-through of Godot Engine's
// servers/rendering/renderer_rd/shaders/effects/screen_space_reflection.glsl
// (https://github.com/godotengine/godot, revision
// d851ae838de54a0bd3dde9043912f39c74265cdd), MIT licensed (LICENSE-godot.txt).
// DFX-20 traces the mirror direction at full importance-sample bias, as that
// file does (revision ed1daf0bf001b61586d9930840f2f1394092c079, 4.7.2-stable).

#include "ScreenSpaceReflectionStructures.fxh"
#include "BasicStructures.fxh"
#include "PBR_Common.fxh"
#include "SSR_Common.fxh"
#include "FullScreenTriangleVSOutput.fxh"

// cbuffer cbCameraAttribs
@group(0) @binding(0) var<uniform> g_Camera: CameraAttribs;

// cbuffer cbScreenSpaceReflectionAttribs
@group(0) @binding(1) var<uniform> g_SSRAttribs: ScreenSpaceReflectionAttribs;

struct PSOutput
{
    @location(0) Specular:     vec4<f32>,
    @location(1) DirectionPDF: vec4<f32>,
}

@group(0) @binding(2) var g_TextureRadiance: texture_2d<f32>;
@group(0) @binding(3) var g_TextureNormal: texture_2d<f32>;
@group(0) @binding(4) var g_TextureRoughness: texture_2d<f32>;
#if SSR_OPTION_PREVIOUS_FRAME
// WGSL: declared only where the host binds it (FEATURE_FLAG_PREVIOUS_FRAME).
@group(0) @binding(5) var g_TextureMotion: texture_2d<f32>;
#endif

@group(0) @binding(6) var g_TextureBlueNoise: texture_2d<f32>;
@group(0) @binding(7) var g_TextureDepthHierarchy: texture_2d<f32>;

fn GetMipResolution(ScreenDimensions: vec2<f32>, MipLevel: i32) -> vec2<f32>
{
    return ScreenDimensions * (1.0 / f32(1i << u32(MipLevel)));
}

fn LoadRandomVector2D(PixelCoord: vec2<i32>) -> vec2<f32>
{
    return HlslLoad(g_TextureBlueNoise, PixelCoord & vec2<i32>(127), 0).xy;
}

fn LoadRoughness(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoad(g_TextureRoughness, PixelCoord, 0).x;
}

fn LoadNormalWS(PixelCoord: vec2<i32>) -> vec3<f32>
{
    return HlslLoad(g_TextureNormal, PixelCoord, 0).xyz;
}

fn LoadDepthHierarchy(PixelCoord: vec2<i32>, MipLevel: i32) -> f32
{
    return HlslLoad(g_TextureDepthHierarchy, PixelCoord, MipLevel).x;
}

#if SSR_OPTION_PREVIOUS_FRAME
fn LoadMotion(PixelCoord: vec2<i32>) -> vec2<f32>
{
    return HlslLoad(g_TextureMotion, PixelCoord, 0).xy * F3NDC_XYZ_TO_UVD_SCALE.xy;
}
#endif

fn LoadRadiance(PixelCoord: vec2<i32>) -> vec3<f32>
{
    return HlslLoad(g_TextureRadiance, PixelCoord, 0).xyz;
}

fn InitialAdvanceRay(Origin: vec3<f32>,
                     Direction: vec3<f32>,
                     InvDirection: vec3<f32>,
                     CurrentMipResolution: vec2<f32>,
                     InvCurrentMipResolution: vec2<f32>,
                     FloorOffset: vec2<f32>,
                     UVOffset: vec2<f32>,
                     Position: ptr<function, vec3<f32>>,
                     CurrentT: ptr<function, f32>)
{
    let CurrentMipPosition = CurrentMipResolution * Origin.xy;

    // Intersect ray with the half box that is pointing away from the ray origin.
    var XYPlane = floor(CurrentMipPosition) + FloorOffset;
    XYPlane = XYPlane * InvCurrentMipResolution + UVOffset;

    // o + d * t = p' => t = (p' - o) / d
    let T = XYPlane * InvDirection.xy - Origin.xy * InvDirection.xy;
    *CurrentT = min(T.x, T.y);
    *Position = Origin + *CurrentT * Direction;
}

fn AdvanceRay(Origin: vec3<f32>,
              Direction: vec3<f32>,
              InvDirection: vec3<f32>,
              CurrentMipPosition: vec2<f32>,
              InvCurrentMipResolution: vec2<f32>,
              FloorOffset: vec2<f32>,
              UVOffset: vec2<f32>,
              SurfaceDepth: f32,
              Position: ptr<function, vec3<f32>>,
              CurrentT: ptr<function, f32>) -> bool
{
    // Create boundary planes
    var XYPlane = floor(CurrentMipPosition) + FloorOffset;
    XYPlane = XYPlane * InvCurrentMipResolution + UVOffset;
    let BoundaryPlanes = vec3<f32>(XYPlane, SurfaceDepth);

    // Intersect ray with the half box that is pointing away from the ray origin.
    // o + d * t = p' => t = (p' - o) / d
    var T = BoundaryPlanes * InvDirection - Origin * InvDirection;

    // Prevent using z plane when shooting out of the depth buffer.
#if SSR_OPTION_INVERTED_DEPTH
    T.z = select(FLT_MAX, T.z, Direction.z < 0.0);
#else
    T.z = select(FLT_MAX, T.z, Direction.z > 0.0);
#endif

    // Choose nearest intersection with a boundary.
    let TMin = min(min(T.x, T.y), T.z);

#if SSR_OPTION_INVERTED_DEPTH
    // Larger z means closer to the camera.
    let AboveSurface = SurfaceDepth < (*Position).z;
#else
    // Smaller z means closer to the camera.
    let AboveSurface = SurfaceDepth > (*Position).z;
#endif

    // Decide whether we are able to advance the ray until we hit the xy boundaries or if we had to clamp it at the surface.
    // We use the asuint comparison to avoid NaN / Inf logic, also we actually care about bitwise equality here to see if t_min is the t.z we fed into the min3 above.
    let SkippedTile = bitcast<u32>(TMin) != bitcast<u32>(T.z) && AboveSurface;

    // Make sure to only advance the ray if we're still above the surface.
    *CurrentT = select(*CurrentT, TMin, AboveSurface);

    // Advance ray
    *Position = Origin + *CurrentT * Direction;

    return SkippedTile;
}

// DFX-18: whether a ray at the most detailed mip is behind the depth surface by at least the depth
// buffer thickness, the distance at which ValidateHit's confidence reaches zero.
fn IsBehindSurfaceThickness(Position: vec3<f32>, SurfaceDepth: f32) -> bool
{
#if SSR_OPTION_INVERTED_DEPTH
    let BelowSurface = Position.z < SurfaceDepth;
#else
    let BelowSurface = Position.z > SurfaceDepth;
#endif
    if (!BelowSurface || IsBackground(SurfaceDepth)) {
        return false;
    }
    let SurfaceVS = ScreenXYDepthToViewSpace(vec3<f32>(Position.xy, SurfaceDepth), g_Camera.mProj);
    let RayVS = ScreenXYDepthToViewSpace(Position, g_Camera.mProj);
    return distance(SurfaceVS, RayVS) * (1.0 / (SurfaceVS.z + FLT_EPS)) >= g_SSRAttribs.DepthBufferThickness;
}

// Requires origin and direction of the ray to be in screen space [0, 1] x [0, 1]
fn HierarchicalRaymarch(Origin: vec3<f32>, Direction: vec3<f32>, ScreenSize: vec2<f32>, MostDetailedMip: i32, MaxTraversalIntersections: u32, ValidHit: ptr<function, bool>) -> vec3<f32>
{
    let InvDirection = vec3<f32>(
                    select(FLT_MAX, 1.0 / Direction.x, Direction.x != 0.0),
                    select(FLT_MAX, 1.0 / Direction.y, Direction.y != 0.0),
                    select(FLT_MAX, 1.0 / Direction.z, Direction.z != 0.0));

    // Start on mip with highest detail.
    var CurrentMip = MostDetailedMip;

    // Could recompute these every iteration, but it's faster to hoist them out and update them.
    var CurrentMipResolution = GetMipResolution(ScreenSize, CurrentMip);
    var InvCurrentMipResolution = 1.0 / CurrentMipResolution;

    // Offset to the bounding boxes uv space to intersect the ray with the center of the next pixel.
    // This means we ever so slightly over shoot into the next region.
    var UVOffset = 0.005 * f32(1i << u32(MostDetailedMip)) / ScreenSize;
    UVOffset.x = select(UVOffset.x, -UVOffset.x, Direction.x < 0.0f);
    UVOffset.y = select(UVOffset.y, -UVOffset.y, Direction.y < 0.0f);

    // Offset applied depending on current mip resolution to move the boundary to the left/right upper/lower border depending on ray direction.
    var FloorOffset: vec2<f32>;
    FloorOffset.x = select(1.0f, 0.0f, Direction.x < 0.0f);
    FloorOffset.y = select(1.0f, 0.0f, Direction.y < 0.0f);

    // Initially advance ray to avoid immediate self intersections.
    var CurrentT: f32;
    var Position: vec3<f32>;
    InitialAdvanceRay(Origin, Direction, InvDirection, CurrentMipResolution, InvCurrentMipResolution, FloorOffset, UVOffset, &Position, &CurrentT);

    // DFX-30: at most SSR_MAX_TRAVERSAL_INTERSECTIONS lookups.
    let MostIntersections = min(MaxTraversalIntersections, u32(SSR_MAX_TRAVERSAL_INTERSECTIONS));
    var Idx = 0u;
    while (Idx < MostIntersections && CurrentMip >= MostDetailedMip)
    {
        // DFX-22: stop at the viewport boundary before loading another tile.
        if (any(Position.xy < vec2<f32>(0.0)) || any(Position.xy >= vec2<f32>(1.0))) {
            break;
        }
        // DFX-26: stop at the far plane, where a ray can only miss, as AMD's
        // hybrid traversal does, instead of descending every mip there.
#if SSR_OPTION_INVERTED_DEPTH
        if (Position.z < 1e-6) {
            break;
        }
#else
        if (Position.z > 1.0 - 1e-6) {
            break;
        }
#endif
        let CurrentMipPosition = CurrentMipResolution * Position.xy;
        let SurfaceDepth = LoadDepthHierarchy(vec2<i32>(CurrentMipPosition), CurrentMip);

        // DFX-18: as Godot's hierarchical SSR does, a ray behind the surface by more than the
        // thickness passes behind it to the next cell at the same mip instead of ending there.
        if (CurrentMip == MostDetailedMip && IsBehindSurfaceThickness(Position, SurfaceDepth))
        {
            var XYPlane = floor(CurrentMipPosition) + FloorOffset;
            XYPlane = XYPlane * InvCurrentMipResolution + UVOffset;
            let T = XYPlane * InvDirection.xy - Origin.xy * InvDirection.xy;
            CurrentT = min(T.x, T.y);
            Position = Origin + CurrentT * Direction;
            Idx += 1u;
            continue;
        }

        let SkippedTile = AdvanceRay(Origin, Direction, InvDirection, CurrentMipPosition, InvCurrentMipResolution, FloorOffset, UVOffset, SurfaceDepth, &Position, &CurrentT);

        // Don't increase mip further than this because we did not generate it
        let NextMipIsOutOfRange = SkippedTile && (CurrentMip >= SSR_DEPTH_HIERARCHY_MAX_MIP);
        if (!NextMipIsOutOfRange)
        {
            CurrentMip += select(-1, 1, SkippedTile);
            CurrentMipResolution *= select(2.0, 0.5, SkippedTile);
            InvCurrentMipResolution *= select(0.5, 2.0, SkippedTile);
        }

        Idx += 1u;
    }

    // As upstream, pass unfinished endpoints to ValidateHit's proximity confidence.
    *ValidHit = Idx <= MostIntersections;

    return Position;
}

fn CalculateEdgeVignette(Hit: vec2<f32>, ScreenSize: vec2<f32>) -> f32
{
    let FOV = 0.05 * vec2<f32>(ScreenSize.y / ScreenSize.x, 1.0);
    let Border = smoothstep(vec2<f32>(0.0f, 0.0f), FOV, Hit.xy) * (1.0 - smoothstep(vec2<f32>(1.0f, 1.0f) - FOV, vec2<f32>(1.0f, 1.0f), Hit.xy));
    return Border.x * Border.y;
}

#if SSR_OPTION_PREVIOUS_FRAME
fn ValidateHit(Hit: vec3<f32>, HitPrev: vec2<f32>, ScreenCoordUV: vec2<f32>, RayDirectionWS: vec3<f32>, ScreenSize: vec2<f32>, DepthBufferThickness: f32) -> f32
#else
fn ValidateHit(Hit: vec3<f32>, ScreenCoordUV: vec2<f32>, RayDirectionWS: vec3<f32>, ScreenSize: vec2<f32>, DepthBufferThickness: f32) -> f32
#endif
{
    // Reject hits outside the view frustum
    if (Hit.x < 0.0f || Hit.y < 0.0f || Hit.x > 1.0f || Hit.y > 1.0f) {
        return 0.0;
    }

    // Reject the hit if we didn't advance the ray significantly to avoid immediate self reflection
    let ManhattanDist = abs(Hit.xy - ScreenCoordUV);
    if (ManhattanDist.x < (2.0f / ScreenSize.x) && ManhattanDist.y < (2.0f / ScreenSize.y)) {
        return 0.0;
    }

    // Don't lookup radiance from the background.
    let TexelCoords = vec2<i32>(ScreenSize * Hit.xy);
    let SurfaceDepth = LoadDepthHierarchy(TexelCoords, 0);

    if (IsBackground(SurfaceDepth)) {
        return 0.0;
    }

    // We check if we hit the surface from the back, these should be rejected.
    let HitNormalWS = LoadNormalWS(TexelCoords);
    if (dot(HitNormalWS, RayDirectionWS) > 0.0) {
        return 0.0;
    }

    let SurfaceVS = ScreenXYDepthToViewSpace(vec3<f32>(Hit.xy, SurfaceDepth), g_Camera.mProj);
    let HitVS = ScreenXYDepthToViewSpace(Hit, g_Camera.mProj);
    let Distance = distance(SurfaceVS, HitVS);

    // Fade out hits near the screen borders
#if SSR_OPTION_PREVIOUS_FRAME
    let Vignette = min(CalculateEdgeVignette(HitPrev.xy, ScreenSize), CalculateEdgeVignette(Hit.xy, ScreenSize));
#else
    let Vignette = CalculateEdgeVignette(Hit.xy, ScreenSize);
#endif

    // We accept all hits that are within a reasonable minimum screen-space distance below the surface.
    // Add constant in linear space to avoid growing of the reflections towards the reflected objects.
    var Confidence = 1.0f - smoothstep(0.0f, DepthBufferThickness, Distance * (1.0 / (SurfaceVS.z + FLT_EPS)));
    Confidence *= Confidence;

    return Vignette * Confidence;
}

fn SmithGGXSampleVisibleNormalHemisphere(View: vec3<f32>, Alpha: f32, Xi: vec2<f32>) -> vec3<f32>
{
    return SmithGGXSampleVisibleNormalSC(View, Alpha, Alpha, Xi.x, Xi.y);
}

fn SampleReflectionVector(View: vec3<f32>, Normal: vec3<f32>, Roughness: f32, PixelCoord: vec2<i32>) -> vec4<f32>
{
    let AlphaRoughness = Roughness * Roughness;
    let N = Normal;
    let T = normalize(cross(N, select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(N.y) > 0.5)));
    let B = cross(T, N);
    let TangentToWorld = MatrixFromRows_f3(T, B, N);

    var Xi = LoadRandomVector2D(PixelCoord);
    Xi.y = mix(Xi.y, 0.0, g_SSRAttribs.GGXImportanceSampleBias);

    let ViewDirTS = View * TangentToWorld;
    // DFX-20: at full bias the ray follows the lobe's peak, the mirror direction, as Godot's SSR
    // traces reflect(view, normal). The fully biased sample is the spherical cap's pole, which is not.
    var MicroNormalTS = vec3<f32>(0.0, 0.0, 1.0);
    if (g_SSRAttribs.GGXImportanceSampleBias < 1.0) {
        MicroNormalTS = SmithGGXSampleVisibleNormalHemisphere(ViewDirTS, AlphaRoughness, Xi);
    }
    let SampleDirTS = reflect(-ViewDirTS, MicroNormalTS);

    // Normal sampled with PDF: Dv(Ne) / (4 * dot(Ve, Ne))
    // Dv(Ne) = G1(Ve) * max(0, dot(Ve, Ne)) * D(Ne) / Ve.z
    let NdotV = ViewDirTS.z;
    let NdotH = MicroNormalTS.z;

    let D = NormalDistribution_GGX(NdotH, AlphaRoughness);
    let G1 = SmithGGXMasking(NdotV, AlphaRoughness);
    let PDF = G1 * D / (4.0 * NdotV + FLT_EPS);
    return vec4<f32>(TangentToWorld * SampleDirTS, PDF);
}

@fragment
fn ComputeIntersectionPS(VSOut: FullScreenTriangleVSOutput) -> PSOutput
{
#if SSR_OPTION_HALF_RESOLUTION
    let SampleIdx = ComputeHalfResolutionOffset(vec2<u32>(VSOut.f4PixelPos.xy));
    let Position = 2.0 * floor(VSOut.f4PixelPos.xy) + vec2<f32>(f32(SampleIdx & 0x01u), f32(SampleIdx >> 1u)) + 0.5;
#else
    let Position = VSOut.f4PixelPos.xy;
#endif

    let ScreenCoordUV = Position * g_Camera.f4ViewportSize.zw;
    let NormalWS = LoadNormalWS(vec2<i32>(Position));
    let NormalVS = (g_Camera.mView * vec4<f32>(NormalWS, 0.0)).xyz;
    let Roughness = LoadRoughness(vec2<i32>(Position));

    let IsMirror = IsMirrorReflection(Roughness);
    let MostDetailedMip = select(i32(g_SSRAttribs.MostDetailedMip), 0, IsMirror);
    let MipResolution = GetMipResolution(g_Camera.f4ViewportSize.xy, MostDetailedMip);

    let RayOriginSS = vec3<f32>(ScreenCoordUV, LoadDepthHierarchy(vec2<i32>(ScreenCoordUV * MipResolution), MostDetailedMip));
    let RayOriginVS = ScreenXYDepthToViewSpace(RayOriginSS, g_Camera.mProj);

    let RayDirectionVS = SampleReflectionVector(-normalize(RayOriginVS), NormalVS, Roughness, vec2<i32>(VSOut.f4PixelPos.xy));
    let RayDirectionSS = ProjectDirection(RayOriginVS, RayDirectionVS.xyz, RayOriginSS, g_Camera.mProj);
    let RayDirectionWS = (g_Camera.mViewInv * vec4<f32>(RayDirectionVS.xyz, 0.0)).xyz;

    var ValidHit = false;
    let SurfaceHitSS = HierarchicalRaymarch(RayOriginSS, RayDirectionSS, g_Camera.f4ViewportSize.xy, MostDetailedMip, g_SSRAttribs.MaxTraversalIntersections, &ValidHit);
    let SurfaceHitVS = ScreenXYDepthToViewSpace(SurfaceHitSS, g_Camera.mProj);

#if SSR_OPTION_PREVIOUS_FRAME
    let Motion = LoadMotion(vec2<i32>(g_Camera.f4ViewportSize.xy * SurfaceHitSS.xy));
    let SurfaceHitSSPrev = SurfaceHitSS.xy - Motion;
    let Confidence = select(0.0, ValidateHit(SurfaceHitSS, SurfaceHitSSPrev, ScreenCoordUV, RayDirectionWS, g_Camera.f4ViewportSize.xy, g_SSRAttribs.DepthBufferThickness), ValidHit);
    let ReflectionRadiance = select(vec3<f32>(0.0, 0.0, 0.0), LoadRadiance(vec2<i32>(g_Camera.f4ViewportSize.xy * SurfaceHitSSPrev)), Confidence > 0.0f);
#else
    let Confidence = select(0.0, ValidateHit(SurfaceHitSS, ScreenCoordUV, RayDirectionWS, g_Camera.f4ViewportSize.xy, g_SSRAttribs.DepthBufferThickness), ValidHit);
    let ReflectionRadiance = select(vec3<f32>(0.0, 0.0, 0.0), LoadRadiance(vec2<i32>(g_Camera.f4ViewportSize.xy * SurfaceHitSS.xy)), Confidence > 0.0f);
#endif

    //TODO: Try to store inverse RayDirectionWS for more accuracy.
    var Output: PSOutput;
    // DFX-17: radiance premultiplied by confidence, so averages of hits and misses keep both consistent.
    Output.Specular = vec4<f32>(ReflectionRadiance * Confidence, Confidence);
    Output.DirectionPDF = vec4<f32>(RayDirectionWS * distance(SurfaceHitVS, RayOriginVS), RayDirectionVS.w);
    return Output;
}
