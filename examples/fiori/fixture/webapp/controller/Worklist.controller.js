sap.ui.define([
    "sap/ui/core/mvc/Controller",
    "sap/ui/model/Filter",
    "sap/ui/model/FilterOperator",
    "sap/m/MessageToast",
    "sap/m/Dialog",
    "sap/m/Button",
    "sap/m/Text",
    "sap/m/Popover"
], function (Controller, Filter, FilterOperator, MessageToast, Dialog, Button, Text, Popover) {
    "use strict";

    return Controller.extend("fixture.controller.Worklist", {

        onSearch: function (oEvent) {
            var sQuery = oEvent.getParameter("query") || oEvent.getParameter("newValue") || "";
            var oTable = this.byId("supplierTable");
            var oBinding = oTable.getBinding("items");
            if (!sQuery) {
                oBinding.filter([]);
                return;
            }
            oBinding.filter([
                new Filter({
                    filters: [
                        new Filter("Name", FilterOperator.Contains, sQuery),
                        new Filter("City", FilterOperator.Contains, sQuery)
                    ],
                    and: false
                })
            ]);
        },

        onSupplierPress: function (oEvent) {
            var oItem = oEvent.getSource();
            var oContext = oItem.getBindingContext();
            var sSupplierId = oContext.getProperty("SupplierID");
            this.getOwnerComponent().getRouter().navTo("detail", { supplierId: sSupplierId });
        },

        // A `sap.m.Dialog` - renders into the `sap-ui-static` UIArea, a
        // sibling of the app's own DOM root (fixture: exercises H4).
        onDeletePress: function () {
            var oTable = this.byId("supplierTable");
            var aSelected = oTable.getSelectedItems();
            if (!this._oDeleteDialog) {
                this._oDeleteDialog = new Dialog({
                    id: this.createId("deleteConfirmDialog"),
                    title: "Confirm delete",
                    type: "Message",
                    content: new Text({
                        id: this.createId("deleteConfirmText"),
                        text: "Delete the selected supplier(s)? This cannot be undone."
                    }),
                    beginButton: new Button({
                        id: this.createId("deleteConfirmYes"),
                        text: "Delete",
                        type: "Reject",
                        press: function () {
                            MessageToast.show(aSelected.length + " supplier(s) deleted.");
                            oTable.removeSelections(true);
                            this._oDeleteDialog.close();
                        }.bind(this)
                    }),
                    endButton: new Button({
                        id: this.createId("deleteConfirmNo"),
                        text: "Cancel",
                        press: function () {
                            this._oDeleteDialog.close();
                        }.bind(this)
                    })
                });
                this.getView().addDependent(this._oDeleteDialog);
            }
            this._oDeleteDialog.open();
        },

        // A `sap.m.Popover` - also static-UIArea content, anchored to a
        // trigger control rather than centered like the Dialog above.
        onInfoPress: function (oEvent) {
            if (!this._oInfoPopover) {
                this._oInfoPopover = new Popover({
                    id: this.createId("infoPopover"),
                    title: "About this list",
                    contentWidth: "18rem",
                    content: new Text({
                        id: this.createId("infoPopoverText"),
                        text: "flowproof Fiori fixture - a local, offline UI5 app used to evaluate authoring reliability."
                    })
                });
                this.getView().addDependent(this._oInfoPopover);
            }
            this._oInfoPopover.openBy(oEvent.getSource());
        }
    });
});
