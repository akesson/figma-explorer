# VariableCollectionCreate

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**action** | **String** | The action to perform for the variable collection. | 
**id** | Option<**String**> | A temporary id for this variable collection. | [optional]
**name** | **String** | The name of this variable collection. | 
**initial_mode_id** | Option<**String**> | The initial mode refers to the mode that is created by default. You can set a temporary id here, in order to reference this mode later in this request. | [optional]
**hidden_from_publishing** | Option<**bool**> | Whether this variable collection is hidden when publishing the current file as a library. | [optional][default to false]
**parent_variable_collection_id** | Option<**String**> | The id of the parent variable collection that this variable collection is extending from. | [optional]
**initial_mode_id_to_parent_mode_id_mapping** | Option<**std::collections::HashMap<String, String>**> | Maps inherited modes from the parent variable collection to the initial mode ids on the extended variable collection. | [optional]

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


