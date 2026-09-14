sap.ui.define([
    "sap/ui/core/UIComponent",
    "sap/ui/model/json/JSONModel"
], function (UIComponent, JSONModel) {
    "use strict";

    return UIComponent.extend("fixture.Component", {

        metadata: {
            manifest: "json"
        },

        // The MockServer is armed and its promise awaited in index.html,
        // *before* this component (and the manifest-declared OData model
        // that starts requesting the instant UIComponent.init() runs) is
        // ever instantiated - see model/mockserver.js for why the ordering
        // has to be that way round rather than awaited in here.
        init: function () {
            UIComponent.prototype.init.apply(this, arguments);

            this.setModel(new JSONModel({ busy: false }), "app");

            this.getRouter().initialize();
        }
    });
});
