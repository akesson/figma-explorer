# PostVariablesRequest

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**variable_collections** | Option<[**Vec<models::VariableCollectionChange>**](VariableCollectionChange.md)> | For creating, updating, and deleting variable collections. | [optional]
**variable_modes** | Option<[**Vec<models::VariableModeChange>**](VariableModeChange.md)> | For creating, updating, and deleting modes within variable collections. | [optional]
**variables** | Option<[**Vec<models::VariableChange>**](VariableChange.md)> | For creating, updating, and deleting variables. | [optional]
**variable_mode_values** | Option<[**Vec<models::VariableModeValue>**](VariableModeValue.md)> | For setting a specific value, given a variable and a mode. | [optional]

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


