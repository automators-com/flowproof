sap.ui.define([
    "sap/ui/core/util/MockServer"
], function (MockServer) {
    "use strict";

    var oMockServer;
    var SERVICE_URL = "/sap/opu/odata/sap/FLOWPROOF_FIXTURE_SRV/";

    return {
        /**
         * Arms a MockServer for the fixture's OData v2 service. Latency and
         * error injection are controlled by URL params so the eval harness
         * can drive them without touching app code:
         *   ?mockDelay=<ms>   - fixed delay added to every response
         *   ?mockErrorRate=<0..1> - fraction of GET requests answered 500
         * Both default to off (0), matching a fast local backend.
         */
        // `MockServer.simulate()` reads the metadata file itself before it
        // can register mock routes - that read is asynchronous, so anything
        // that depends on the routes being live (in practice: the manifest's
        // OData model, which starts firing $metadata/read requests the
        // instant UIComponent.prototype.init() runs) must wait for this
        // promise rather than for `init()` to merely return. Bootstrapping
        // the component itself only after this resolves (see index.html) is
        // what makes that ordering hold, matching the pattern in UI5's own
        // reference apps rather than inventing a different one.
        init: function () {
            if (oMockServer) {
                return Promise.resolve();
            }

            var oUriParams = new URLSearchParams(window.location.search);
            var iDelay = parseInt(oUriParams.get("mockDelay"), 10) || 0;
            var fErrorRate = parseFloat(oUriParams.get("mockErrorRate")) || 0;

            return new Promise(function (fnResolve) {
                var sMetadataUrl = sap.ui.require.toUrl("fixture/localService/metadata.xml");

                MockServer.config({ autoRespond: true, autoRespondAfter: iDelay });

                oMockServer = new MockServer({ rootUri: SERVICE_URL });
                oMockServer.simulate(sMetadataUrl, {
                    sMockdataBaseUrl: sap.ui.require.toUrl("fixture/localService"),
                    bGenerateMissingMockData: false
                });

                if (fErrorRate > 0) {
                    oMockServer.getRequests().forEach(function (oRequest) {
                        var fnOriginalResponse = oRequest.response;
                        oRequest.response = function (oXhr) {
                            if (Math.random() < fErrorRate) {
                                oXhr.respond(500, { "Content-Type": "text/plain" }, "flowproof fixture: injected error");
                                return;
                            }
                            fnOriginalResponse.apply(this, arguments);
                        };
                    });
                }

                oMockServer.start();

                // simulate() itself is synchronous against the already-local
                // metadata.xml in this UI5 version, but resolving via a real
                // request round trip (rather than resolving immediately)
                // keeps this robust if that internal is ever made async, and
                // documents the ordering constraint at the one place a
                // future edit is likely to break it.
                jQuery.ajax(sMetadataUrl).always(fnResolve);
            });
        },

        stop: function () {
            if (oMockServer) {
                oMockServer.stop();
                oMockServer = undefined;
            }
        }
    };
});
