sap.ui.define([
    "sap/ui/core/mvc/Controller",
    "sap/m/MessageToast",
    "sap/m/Dialog",
    "sap/m/List",
    "sap/m/StandardListItem",
    "sap/m/SearchField",
    "sap/m/Bar",
    "sap/ui/model/Filter",
    "sap/ui/model/FilterOperator",
    "sap/ui/model/json/JSONModel"
], function (Controller, MessageToast, Dialog, List, StandardListItem, SearchField, Bar, Filter, FilterOperator, JSONModel) {
    "use strict";

    return Controller.extend("fixture.controller.Detail", {

        onInit: function () {
            this.getOwnerComponent().getRouter().getRoute("detail").attachPatternMatched(this._onRouteMatched, this);
        },

        // Routing means this view is instantiated on demand - a fresh
        // `__xmlviewN` counter each time the route is entered again, which is
        // the mechanism H1 is about: whatever DOM id the recorder captured on
        // a prior visit will not match on the next one.
        _onRouteMatched: function (oEvent) {
            var sSupplierId = oEvent.getParameter("arguments").supplierId;
            var oView = this.getView();
            var oModel = this.getOwnerComponent().getModel();
            var oAppModel = this.getOwnerComponent().getModel("app");

            oAppModel.setProperty("/busy", true);
            oModel.read("/Suppliers('" + sSupplierId + "')", {
                success: function (oData) {
                    oAppModel.setProperty("/busy", false);
                    oView.setBindingContext(oModel.createBindingContext(
                        "/Suppliers('" + sSupplierId + "')"
                    ));
                },
                error: function () {
                    oAppModel.setProperty("/busy", false);
                    MessageToast.show("Could not load supplier " + sSupplierId);
                }
            });
        },

        onNavBack: function () {
            this.getOwnerComponent().getRouter().navTo("worklist");
        },

        onSavePress: function () {
            var oAppModel = this.getOwnerComponent().getModel("app");
            oAppModel.setProperty("/busy", true);
            // The mock backend has no real persistence; the round trip
            // itself (and its latency, when the harness injects some via
            // ?mockDelay) is what this button exists to exercise.
            setTimeout(function () {
                oAppModel.setProperty("/busy", false);
                MessageToast.show("Supplier saved.");
            }, 300);
        },

        // A minimal F4 value-help: a searchable list in a Dialog. Not the
        // full `sap.ui.comp.valuehelpdialog.ValueHelpDialog` (that library
        // isn't part of this fixture's dependency set - deliberately kept
        // small; noted as a simplification in the fixture README, not
        // hidden), but it exercises the same shape a real F4 does: opened
        // from a field's value-help icon, renders into the static UIArea,
        // and writes its selection back into the triggering field on close.
        onProductValueHelpRequest: function (oEvent) {
            var oInput = oEvent.getSource();
            var oModel = this.getOwnerComponent().getModel();

            if (!this._oValueHelpDialog) {
                var oList = new List({
                    id: this.createId("productValueHelpList"),
                    mode: "SingleSelectMaster",
                    items: {
                        path: "/Products",
                        template: new StandardListItem({
                            title: "{Name}",
                            description: "{ProductID}",
                            info: "{Category}"
                        })
                    },
                    selectionChange: function (oSelEvent) {
                        var oContext = oSelEvent.getParameter("listItem").getBindingContext();
                        oInput.setValue(oContext.getProperty("ProductID"));
                        this._oValueHelpDialog.close();
                    }.bind(this)
                });
                oList.setModel(oModel);

                this._oValueHelpDialog = new Dialog({
                    id: this.createId("productValueHelpDialog"),
                    title: "Select preferred product",
                    contentWidth: "28rem",
                    contentHeight: "24rem",
                    subHeader: new Bar({
                        id: this.createId("productValueHelpBar"),
                        contentMiddle: new SearchField({
                            id: this.createId("productValueHelpSearch"),
                            width: "100%",
                            search: function (oSearchEvent) {
                                var sQuery = oSearchEvent.getParameter("query") || "";
                                oList.getBinding("items").filter(
                                    sQuery ? [new Filter("Name", FilterOperator.Contains, sQuery)] : []
                                );
                            }
                        })
                    }),
                    content: oList,
                    endButton: new (sap.m.Button)({
                        id: this.createId("productValueHelpCancel"),
                        text: "Cancel",
                        press: function () {
                            this._oValueHelpDialog.close();
                        }.bind(this)
                    })
                });
                this.getView().addDependent(this._oValueHelpDialog);
            }
            this._oValueHelpDialog.open();
        }
    });
});
