// Replacement for "@tauri-apps/plugin-dialog".
// open() lets the user pick a local file, uploads it to the server and
// resolves with the upload id, which the server accepts as `appPath`.
import { toast } from "sonner";
import i18n from "../i18next";
import { UNAUTHORIZED_EVENT } from "./http";

export type DialogFilter = { name: string; extensions: string[] };
export type OpenDialogOptions = {
  multiple?: boolean;
  filters?: DialogFilter[];
  title?: string;
};

function pickFile(accept: string): Promise<File | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.style.display = "none";
    let done = false;
    const finish = (f: File | null) => {
      if (done) return;
      done = true;
      input.remove();
      resolve(f);
    };
    input.addEventListener("change", () => finish(input.files?.[0] ?? null));
    input.addEventListener("cancel", () => finish(null));
    document.body.appendChild(input);
    input.click();
  });
}

export function uploadFile(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    const toastId = toast.loading(
      i18n.t("web.uploading", {
        name: file.name,
        percent: 0,
        defaultValue: "Uploading {{name}}… {{percent}}%",
      }),
    );
    xhr.open("POST", "/api/upload");
    xhr.withCredentials = true;
    xhr.setRequestHeader("Content-Type", "application/octet-stream");
    xhr.upload.onprogress = (e) => {
      if (!e.lengthComputable) return;
      toast.loading(
        i18n.t("web.uploading", {
          name: file.name,
          percent: Math.round((e.loaded / e.total) * 100),
          defaultValue: "Uploading {{name}}… {{percent}}%",
        }),
        { id: toastId },
      );
    };
    xhr.onload = () => {
      toast.dismiss(toastId);
      if (xhr.status === 401) window.dispatchEvent(new Event(UNAUTHORIZED_EVENT));
      let body: any = null;
      try {
        body = JSON.parse(xhr.responseText);
      } catch {
        /* ignore */
      }
      if (xhr.status >= 200 && xhr.status < 300 && body?.id) {
        resolve(body.id as string);
      } else {
        const message = body?.message ?? `HTTP ${xhr.status}`;
        toast.error(
          i18n.t("web.upload_failed", {
            error: message,
            defaultValue: "Upload failed: {{error}}",
          }),
        );
        reject({ type: "upload", message });
      }
    };
    xhr.onerror = () => {
      toast.dismiss(toastId);
      const message = "Network error while uploading";
      toast.error(message);
      reject({ type: "upload", message });
    };
    xhr.send(file);
  });
}

export async function open(options: OpenDialogOptions = {}): Promise<string | null> {
  const accept = (options.filters ?? [])
    .flatMap((f) => f.extensions.map((e) => `.${e}`))
    .join(",");
  const file = await pickFile(accept);
  if (!file) return null;
  try {
    return await uploadFile(file);
  } catch {
    return null;
  }
}

export async function ask(message: string, _options?: unknown): Promise<boolean> {
  return window.confirm(message);
}
