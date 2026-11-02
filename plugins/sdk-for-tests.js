// Serves `@wakaka6/anya-plugin` from this repository's SDK, as Anya's plugin
// host serves its own copy, so the plugins' tests run against the SDK they ship with.
import { plugin } from "bun";
import * as sdk from "../sdk/plugin/index.js";

plugin({
  name: "anya-sdk",
  setup: (build) => build.module("@wakaka6/anya-plugin", () => ({ exports: { ...sdk }, loader: "object" })),
});
