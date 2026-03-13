new Promise((resolve) => {
    let resolved = false;
    const done = (val) => { if (!resolved) { resolved = true; resolve(val); } };

    // Best-effort: resolve after 15s even if network is still busy
    setTimeout(() => done(true), 15000);

    const waitForReady = () => {
        if (document.readyState === 'complete') {
            waitForNetworkIdle();
        } else {
            window.addEventListener('load', waitForNetworkIdle);
        }
    };

    const waitForNetworkIdle = () => {
        let pendingRequests = 0;
        let lastActivity = Date.now();
        const IDLE_THRESHOLD_MS = 500;

        // Intercept fetch
        const originalFetch = window.fetch;
        window.fetch = function(...args) {
            pendingRequests++;
            lastActivity = Date.now();
            return originalFetch.apply(this, args).finally(() => {
                pendingRequests--;
                lastActivity = Date.now();
            });
        };

        // Intercept XHR
        const originalOpen = XMLHttpRequest.prototype.open;
        const originalSend = XMLHttpRequest.prototype.send;
        XMLHttpRequest.prototype.open = function(...args) {
            this._tracked = true;
            return originalOpen.apply(this, args);
        };
        XMLHttpRequest.prototype.send = function(...args) {
            if (this._tracked) {
                pendingRequests++;
                lastActivity = Date.now();
                this.addEventListener('loadend', () => {
                    pendingRequests--;
                    lastActivity = Date.now();
                });
            }
            return originalSend.apply(this, args);
        };

        const checkIdle = () => {
            if (resolved) return;
            const timeSinceActivity = Date.now() - lastActivity;
            if (pendingRequests === 0 && timeSinceActivity >= IDLE_THRESHOLD_MS) {
                done(true);
            } else {
                setTimeout(checkIdle, 100);
            }
        };

        // Start checking after a short delay to catch initial requests
        setTimeout(checkIdle, 100);
    };

    waitForReady();
})
