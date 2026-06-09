// Offline bundle entry for the SpecSlice graph viewer.
// esbuild rolls three + 3d-force-graph + UnrealBloomPass into one IIFE so the
// viewer runs from file:// with no network (classic scripts load locally; ESM
// modules do not). A single bundled `three` instance is shared by the graph
// renderer and the bloom pass — esbuild dedupes by module path.
import * as THREE from "three";
import ForceGraph3D from "3d-force-graph";
import { UnrealBloomPass } from "three/examples/jsm/postprocessing/UnrealBloomPass.js";

globalThis.THREE = THREE;
globalThis.ForceGraph3D = ForceGraph3D;
globalThis.UnrealBloomPass = UnrealBloomPass;
