# PaymentInformation

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**user_id** | **String** | The ID of the user whose payment information was queried. Can be used to verify the validity of a response. | 
**resource_id** | **String** | The ID of the plugin, widget, or Community file that was queried. Can be used to verify the validity of a response. | 
**resource_type** | **String** | The type of the resource. | 
**payment_status** | [**models::PaymentStatus**](PaymentStatus.md) |  | 
**date_of_purchase** | Option<**String**> | The UTC ISO 8601 timestamp indicating when the user purchased the resource. No value is given if the user has never purchased the resource.    Note that a value will still be returned if the user had purchased the resource, but no longer has active access to it (e.g. purchase refunded, subscription ended). | [optional]

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


