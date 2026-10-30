import { defineEngine } from "@anyahq/plugin";

// Streaming: a partial result with the byte count for every chunk of audio,
// the final result when the audio ends. Settings: `prefix` goes before the
// count; `fail` set to `connect` makes `open` fail, to see how Anya shows it.
export default defineEngine({
  manifest: {
    id: "echo",
    title: { en: "Echo", "zh-CN": "回声" },
    blurb: "Reports how many bytes of audio it heard.",
    streaming: true,
    fields: [
      { key: "prefix", label: "Prefix", kind: "text" },
      { key: "fail", label: "Fail on open", kind: "choice", options: ["", "connect"], advanced: true },
    ],
  },

  open({ settings }, session) {
    if (settings.fail === "connect") {
      session.error("connect", "echo was told to fail");
      return;
    }
    const prefix = settings.prefix ?? "";
    let bytes = 0;
    const text = () => `${prefix}${bytes} bytes`;

    session.on("audio", (pcm) => {
      bytes += pcm.length;
      session.result({ text: text(), utterances: [{ text: text(), definite: false }] });
    });
    session.on("finish", () => {
      session.result({ text: text(), utterances: [{ text: text(), definite: true }], final: true });
    });
  },
});
