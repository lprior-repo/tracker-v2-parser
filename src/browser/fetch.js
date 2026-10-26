const request = arguments[0];
const complete = arguments[arguments.length - 1];
const controller = new AbortController();
const timer = setTimeout(() => controller.abort(), 30000);

(async () => {
    if (location.origin !== request.origin) {
        return {error: "origin"};
    }
    const target = new URL(request.url);
    if (target.hostname !== location.hostname) {
        return {error: "host"};
    }
    const response = await fetch(target.href, {
        method: request.method,
        headers: request.headers,
        body: request.body === null ? undefined : request.body,
        credentials: "include",
        redirect: "error",
        signal: controller.signal
    });
    if (response.type === "opaque" || response.type === "opaqueredirect" || response.status === 0) {
        return {error: "unreadable"};
    }
    const chunks = [];
    let length = 0;
    if (response.body !== null) {
        const reader = response.body.getReader();
        let finished = false;
        for (let index = 0; index <= request.max_chunks; index += 1) {
            const chunk = await reader.read();
            if (chunk.done) {
                finished = true;
                break;
            }
            if (index === request.max_chunks) {
                controller.abort();
                return {error: "chunks"};
            }
            length += chunk.value.byteLength;
            if (length > request.max_bytes) {
                controller.abort();
                return {error: "size"};
            }
            chunks.push(chunk.value);
        }
        if (!finished) {
            controller.abort();
            return {error: "chunks"};
        }
    }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) {
        bytes.set(chunk, offset);
        offset += chunk.byteLength;
    }
    const encoded = [];
    for (let start = 0; start < bytes.byteLength; start += 32766) {
        const piece = bytes.subarray(start, Math.min(start + 32766, bytes.byteLength));
        encoded.push(btoa(String.fromCharCode(...piece)));
    }
    return {
        status: response.status,
        content_type: response.headers.get("content-type") || "",
        url: response.url,
        body: encoded.join("")
    };
})().then(complete, error => {
    complete({error: error && error.name === "AbortError" ? "timeout" : "network"});
}).finally(() => {
    clearTimeout(timer);
    controller.abort();
});
