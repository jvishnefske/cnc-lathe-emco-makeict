/**
 * Optional, collapsible webcam pane (getUserMedia). For watching the cut
 * from a step back — NOT for remote operation; nothing here streams
 * anywhere, the video stays in the page.
 */

export class WebcamPane {
  readonly el: HTMLElement;
  private video: HTMLVideoElement;
  private stream: MediaStream | null = null;
  private msg: HTMLElement;

  constructor() {
    this.el = document.createElement("details");
    this.el.className = "panel webcam-pane";
    this.el.innerHTML = `
      <summary class="panel-title">Webcam <span class="webcam-hint">(local view only)</span></summary>
      <div class="webcam-body">
        <video muted playsinline></video>
        <div class="webcam-msg" data-e="msg" hidden></div>
        <div class="webcam-tools">
          <button class="btn btn-small" data-e="start">Start camera</button>
          <button class="btn btn-small" data-e="stop" disabled>Stop</button>
        </div>
      </div>`;
    this.video = this.el.querySelector("video")!;
    this.msg = this.el.querySelector<HTMLElement>('[data-e="msg"]')!;
    const startBtn = this.el.querySelector<HTMLButtonElement>('[data-e="start"]')!;
    const stopBtn = this.el.querySelector<HTMLButtonElement>('[data-e="stop"]')!;
    startBtn.addEventListener("click", () => {
      void this.start().then(
        () => {
          startBtn.disabled = true;
          stopBtn.disabled = false;
        },
        (err: unknown) => {
          this.msg.textContent =
            "Camera unavailable: " + (err instanceof Error ? err.message : String(err));
          this.msg.hidden = false;
        },
      );
    });
    stopBtn.addEventListener("click", () => {
      this.stop();
      startBtn.disabled = false;
      stopBtn.disabled = true;
    });
  }

  private async start(): Promise<void> {
    if (!navigator.mediaDevices?.getUserMedia) {
      throw new Error("getUserMedia not supported in this browser");
    }
    this.msg.hidden = true;
    this.stream = await navigator.mediaDevices.getUserMedia({ video: true, audio: false });
    this.video.srcObject = this.stream;
    await this.video.play();
  }

  stop(): void {
    if (this.stream) {
      for (const t of this.stream.getTracks()) t.stop();
      this.stream = null;
    }
    this.video.srcObject = null;
  }
}
